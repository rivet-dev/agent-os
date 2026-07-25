import { createHash, randomUUID, timingSafeEqual } from "node:crypto";
import { mkdtemp, open, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import appsBuilder, {
	appBundleManifestVersion,
	appsBuilderVersion,
} from "@agentos-software/apps-builder";
import sh from "@agentos-software/sh";
import tar from "@agentos-software/tar";
import {
	agentOS,
	type VmFetchResponse,
	type VmFetchStreamChunk,
	type VmFetchStreamHead,
} from "@rivet-dev/agentos";
import {
	AgentOs,
	type AgentOsOptions,
} from "@rivet-dev/agentos-core";
import { packAospkgFromTarBytes } from "@rivet-dev/agentos-toolchain";
import { actor, UserError } from "rivetkit";
import { db, type RawAccess } from "rivetkit/db";
import {
	configureAppNamespaceRunner,
	resolveDefaultRivetConnection,
} from "./control-plane.js";
import {
	type GuestEngineProxyRegistration,
	registerGuestEngineProxy,
	unregisterGuestEngineProxy,
} from "./engine-proxy.js";
import { AgentOSAppsError } from "./errors.js";
import {
	APP_CALLBACK_SECRET_HEADER,
	canonicalDeploymentHash,
	normalizeAppPath,
	releaseEnvoyVersion,
	runnerSource,
	staticRunnerSource,
} from "./runtime.js";
import type {
	AppReleaseInfo,
	AppScaling,
	Deployment,
	PreparedDeployAppInput,
} from "./types.js";

const APP_PORT = 3_080;
const DEFAULT_MAX_SOURCE_BYTES = 4 * 1024 * 1024;
const DEFAULT_MAX_FILES = 2_000;
const DEFAULT_MAX_VERSIONS = 20;
const DEFAULT_MAX_REGIONS = 8;
const DEFAULT_BUILD_TIMEOUT_MS = 15 * 60_000;
const DEFAULT_WARM_TIMEOUT_MS = 30_000;
const DEFAULT_WARM_IDLE_TIMEOUT_MS = 5 * 60_000;
const DEFAULT_ADMISSION_LEASE_MS = 60_000;
const DEFAULT_MAX_ADMISSIONS = 4_096;
const DEFAULT_MAX_REQUEST_BYTES = 1024 * 1024;
const DEFAULT_MAX_RESPONSE_BYTES = 4 * 1024 * 1024;
const DEFAULT_MAX_SERVERLESS_METADATA_BYTES = 256 * 1024;
const GUEST_SHUTDOWN_TIMEOUT_MS = 15_000;
const GUEST_RPC_TIMEOUT_MS = 30_000;
const MAX_PENDING_GUEST_RPCS = 1_024;
const GUEST_RPC_PREFIX = "AGENTOS_APPS_RPC ";
const DEFAULT_MAX_DEPENDENCIES = 256;
const DEFAULT_MAX_BUILD_OUTPUT_BYTES = 2 * 1024 * 1024;
const DEFAULT_MAX_BUILD_ARTIFACT_BYTES = 64 * 1024 * 1024;
const DEFAULT_MAX_BUILD_ARTIFACT_FILES = 4_096;
const DEFAULT_MAX_BUILD_ARTIFACT_FILE_BYTES = 32 * 1024 * 1024;
const DEFAULT_MAX_BUILD_FILESYSTEM_BYTES = 2 * 1024 * 1024 * 1024;
const MAX_REPLICAS = 128;
const ARTIFACT_CHUNK_BYTES = 512 * 1024;
const ARTIFACT_CHUNKS_PER_TRANSACTION = 1;
const MAX_ARTIFACT_CHUNKS = Math.ceil(
	DEFAULT_MAX_BUILD_ARTIFACT_BYTES / ARTIFACT_CHUNK_BYTES,
);
const SOURCE_CHUNK_BYTES = 512 * 1024;
const MAX_SOURCE_CHUNKS =
	Math.ceil(DEFAULT_MAX_SOURCE_BYTES / SOURCE_CHUNK_BYTES) + DEFAULT_MAX_FILES;

const APP_ACTOR_NAME = "agentOSAppsApp";
const SCALER_ACTOR_NAME = "agentOSAppsScaler";
const REPLICA_ACTOR_NAME = "agentOSAppsReplica";
const INSPECTOR_ROOT = fileURLToPath(
	new URL("../assets/inspector", import.meta.url),
);

export interface StoredAppRelease extends AppReleaseInfo {
	entrypoint: string;
	namespace: string;
	envoyVersion: number;
	runtimeEndpoint: string;
	runtimePool: string;
	usesRivetKit: boolean;
	callbackSecret: string;
}

export interface AppState {
	activeRelease: string | null;
	namespace: string | null;
	revision: number;
	nextEnvoyVersion?: number;
	serverlessMetadata?: {
		release: string;
		status: number;
		statusText: string;
		headers: Record<string, string>;
		bodyBase64: string;
	};
}

export interface ReplicaRecord {
	key: string[];
	readyAt: number;
	activeRequests: number;
	lastUsedAt: number;
	draining: boolean;
}

interface AdmissionLease {
	id: string;
	replicaKey: string[];
	expiresAt: number;
}

export interface ScalerState {
	appId: string | null;
	release: string | null;
	region: string | null;
	scaling: Required<AppScaling> | null;
	replicas: ReplicaRecord[];
	warmingReplicas: number;
	warmingReplicaKeys?: string[][];
	capacityWarningLatched: boolean;
	retired: boolean;
	revision: number;
	selectionCursor: number;
	nextReplicaIndex: number;
	reconcileScheduledAt: number | null;
	admissions?: Record<string, AdmissionLease>;
}

export interface ReplicaState {
	configuration: {
		appId: string;
		release: string;
		artifactHash: string;
		artifactBytes: number;
		namespace: string;
		envoyVersion: number;
		runtime: AppRuntimeConfig;
		/** Absent on replicas persisted before conditional Engine access. */
		usesRivetKit?: boolean;
	} | null;
	startedAt: number | null;
	guestPid: number | null;
}

interface ReplicaAdmission {
	admissionId: string;
	leaseMs: number;
	key: string[];
	release: string;
	region: string;
	replicaCount: number;
	queueDelayMs: number;
	coldStart: boolean;
}

export interface AppRuntimeConfig {
	endpoint: string;
	namespace: string;
	pool: string;
}

type ReplicaConfiguration = NonNullable<ReplicaState["configuration"]>;

/** @internal Exported for focused security tests. */
export function replicaGuestEnvironment(
	configuration: ReplicaConfiguration,
	engineEndpoint = configuration.runtime.endpoint,
): Record<string, string> {
	const env: Record<string, string> = { NODE_ENV: "production" };
	if (!configuration.usesRivetKit) return env;
	return {
		...env,
		RIVETKIT_RUNTIME: "wasm",
		RIVETKIT_RUNTIME_MODE: "serverless",
		RIVET_ENVOY_VERSION: String(
			configuration.envoyVersion ?? releaseEnvoyVersion(configuration.release),
		),
		RIVET_ENDPOINT: engineEndpoint,
		RIVET_NAMESPACE: configuration.runtime.namespace,
		RIVET_POOL: configuration.runtime.pool,
		RIVET_RUNNER: configuration.runtime.pool,
		RIVET_RUNNER_POOL: configuration.runtime.pool,
	};
}

/** @internal Exported for focused security tests. */
export function replicaLoopbackExemptPorts(
	configuration: ReplicaConfiguration,
	proxyPort?: number,
): number[] {
	return configuration.usesRivetKit && proxyPort !== undefined
		? [proxyPort]
		: [];
}

type AnyActorContext = {
	actorId: string;
	key: string[];
	region: string;
	state: any;
	db: RawAccess;
	client(): any;
	keepAwake<T>(promise: Promise<T>): Promise<T>;
	destroy(): void;
	schedule: {
		after(
			delayMs: number,
			action: string,
			...args: unknown[]
		): Promise<unknown>;
	};
	log: {
		info(value: unknown): void;
		warn(value: unknown): void;
		error(value: unknown): void;
	};
};

interface ExecResult {
	exitCode: number;
	stdout: string;
	stderr: string;
}

type BuildHandle = {
	artifactGuestPath: string;
	writeFiles(
		entries: Array<{ path: string; content: string | Uint8Array }>,
	): Promise<Array<{ path: string; success: boolean; error?: string }>>;
	execArgv(
		command: string,
		args: string[],
		options?: {
			cwd?: string;
			env?: Record<string, string>;
			timeout?: number;
			captureStdio?: boolean;
		},
	): Promise<ExecResult>;
	artifactSize(): Promise<number>;
	readArtifact(): Promise<Uint8Array>;
	dispose(): Promise<void>;
};

type ReplicaHandle = {
	destroy(): Promise<void>;
	configure(input: ReplicaState["configuration"]): Promise<void>;
	markStarted(): Promise<void>;
	inspect(): Promise<{
		release: string | null;
		startedAt: number | null;
	}>;
	fetch(
		input: string | URL | Request,
		init?: RequestInit & { skipReadyWait?: boolean },
	): Promise<Response>;
	spawn(
		command: string,
		args: string[],
		options?: { cwd?: string; env?: Record<string, string> },
	): Promise<{ pid: number }>;
	vmFetch(
		port: number,
		url: string,
		options?: {
			method?: string;
			headers?: Record<string, string>;
			body?: string | Uint8Array;
		},
	): Promise<{
		status: number;
		statusText: string;
		headers: Record<string, string>;
		body: Uint8Array;
	}>;
	vmFetchStreamStart(
		port: number,
		url: string,
		options?: {
			method?: string;
			headers?: Record<string, string>;
			body?: string | Uint8Array;
		},
	): Promise<VmFetchStreamHead>;
	vmFetchStreamRead(
		streamId: string,
		maxBytes?: number,
	): Promise<VmFetchStreamChunk>;
	vmFetchStreamCancel(streamId: string): Promise<void>;
};

export interface AppRouteResolution {
	appId: string;
	release: string;
	region: string;
	scalerKey: string[];
	revision: number;
	maxRequestBytes: number;
	maxResponseBytes: number;
}

async function readBoundedRequestBody(
	request: Request,
	maxBytes: number,
): Promise<Uint8Array | null | undefined> {
	if (request.method === "GET" || request.method === "HEAD") return undefined;
	if (!request.body) return new Uint8Array(0);
	const reader = request.body.getReader();
	const chunks: Uint8Array[] = [];
	let bytes = 0;
	try {
		for (;;) {
			const { value, done } = await reader.read();
			if (done) break;
			bytes += value.byteLength;
			if (bytes > maxBytes) {
				await reader.cancel("AgentOS Apps request body limit exceeded");
				return null;
			}
			chunks.push(value);
		}
	} finally {
		reader.releaseLock();
	}
	return new Uint8Array(Buffer.concat(chunks, bytes));
}

async function readBoundedResponseBody(
	response: Response,
	maxBytes: number,
): Promise<Uint8Array> {
	if (!response.body) return new Uint8Array(0);
	const reader = response.body.getReader();
	const chunks: Uint8Array[] = [];
	let bytes = 0;
	try {
		for (;;) {
			const { value, done } = await reader.read();
			if (done) break;
			bytes += value.byteLength;
			if (bytes > maxBytes) {
				await reader.cancel("AgentOS Apps response body limit exceeded");
				fail(
					"agentos_apps_response_limit",
					`response exceeds ${maxBytes} bytes`,
					{ limit: maxBytes },
				);
			}
			chunks.push(value);
		}
	} finally {
		reader.releaseLock();
	}
	return new Uint8Array(Buffer.concat(chunks, bytes));
}

const locks = new Map<string, Promise<void>>();

async function serialized<T>(key: string, run: () => Promise<T>): Promise<T> {
	const previous = locks.get(key) ?? Promise.resolve();
	let release = () => {};
	const gate = new Promise<void>((resolve) => {
		release = resolve;
	});
	const current = previous.then(() => gate);
	locks.set(key, current);
	await previous;
	try {
		return await run();
	} finally {
		release();
		if (locks.get(key) === current) locks.delete(key);
	}
}

function fail(
	code: string,
	message: string,
	metadata?: Record<string, unknown>,
): never {
	throw new UserError(message, { code, metadata });
}

function positiveInteger(value: number, name: string, maximum: number): number {
	if (!Number.isInteger(value) || value < 1 || value > maximum) {
		fail(
			"agentos_apps_invalid_config",
			`${name} must be an integer between 1 and ${maximum}`,
			{ name, maximum },
		);
	}
	return value;
}

export function normalizeScaling(
	input: AppScaling | undefined,
): Required<AppScaling> {
	const minReplicas = input?.minReplicas ?? 0;
	const maxReplicas = input?.maxReplicas ?? 128;
	const targetConcurrency = input?.targetConcurrency ?? 8;
	if (
		!Number.isInteger(minReplicas) ||
		minReplicas < 0 ||
		minReplicas > MAX_REPLICAS
	) {
		fail(
			"agentos_apps_invalid_scaling",
			`scaling.minReplicas must be an integer between 0 and ${MAX_REPLICAS}`,
		);
	}
	positiveInteger(maxReplicas, "scaling.maxReplicas", MAX_REPLICAS);
	positiveInteger(targetConcurrency, "scaling.targetConcurrency", 1_024);
	if (minReplicas > maxReplicas) {
		fail(
			"agentos_apps_invalid_scaling",
			"scaling.minReplicas cannot exceed scaling.maxReplicas",
		);
	}
	return { minReplicas, maxReplicas, targetConcurrency };
}

/** @internal Exported for focused migration tests. */
export async function migrateAppsTables(database: RawAccess): Promise<void> {
	await database.execute(`
		CREATE TABLE IF NOT EXISTS agentos_apps_releases (
			release_id TEXT PRIMARY KEY,
			created_at INTEGER NOT NULL,
			status TEXT NOT NULL,
			entrypoint TEXT NOT NULL,
			artifact_hash TEXT NOT NULL DEFAULT '',
			artifact_bytes INTEGER NOT NULL DEFAULT 0,
			build_error TEXT,
			regions_json TEXT NOT NULL,
			scaling_json TEXT NOT NULL,
				namespace TEXT NOT NULL,
				envoy_version INTEGER NOT NULL,
				runtime_endpoint TEXT NOT NULL,
				runtime_pool TEXT NOT NULL,
				callback_secret TEXT NOT NULL DEFAULT '',
				uses_rivetkit INTEGER NOT NULL DEFAULT 0
					CHECK (uses_rivetkit IN (0, 1))
		);
		CREATE TABLE IF NOT EXISTS agentos_apps_release_files (
			release_id TEXT NOT NULL,
			path TEXT NOT NULL,
			chunk_index INTEGER NOT NULL,
			content BLOB NOT NULL,
			byte_length INTEGER NOT NULL,
			PRIMARY KEY (release_id, path, chunk_index)
		);
		CREATE TABLE IF NOT EXISTS agentos_apps_artifact_chunks (
			release_id TEXT NOT NULL,
			chunk_index INTEGER NOT NULL,
			content BLOB NOT NULL,
			byte_length INTEGER NOT NULL,
			PRIMARY KEY (release_id, chunk_index)
		);
		CREATE INDEX IF NOT EXISTS idx_agentos_apps_releases_created_at
			ON agentos_apps_releases(created_at);
	`);
	const columns = await database.execute<{ name: string }>(
		"PRAGMA table_info(agentos_apps_releases)",
	);
	if (!columns.some((column) => column.name === "callback_secret")) {
		await database.execute(
			`ALTER TABLE agentos_apps_releases
			 ADD COLUMN callback_secret TEXT NOT NULL DEFAULT ''`,
		);
	}
	if (!columns.some((column) => column.name === "uses_rivetkit")) {
		await database.execute(
			`ALTER TABLE agentos_apps_releases
			 ADD COLUMN uses_rivetkit INTEGER NOT NULL DEFAULT 0
			 CHECK (uses_rivetkit IN (0, 1))`,
		);
	}
}

async function deleteReleaseFilesBatched(
	database: RawAccess,
	releaseId: string,
): Promise<void> {
	for (let batch = 0; batch < MAX_SOURCE_CHUNKS; batch += 1) {
		const rows = await database.execute<{ chunks: number }>(
			`SELECT COUNT(*) AS chunks
			 FROM agentos_apps_release_files
			 WHERE release_id = ?`,
			releaseId,
		);
		if (Number(rows[0]?.chunks ?? 0) === 0) return;
		await database.execute(
			`DELETE FROM agentos_apps_release_files
			 WHERE rowid IN (
				SELECT rowid FROM agentos_apps_release_files
				 WHERE release_id = ?
				 ORDER BY path, chunk_index
				 LIMIT 1
			 )`,
			releaseId,
		);
	}
	fail(
		"agentos_apps_source_cleanup_limit",
		`source cleanup exceeded ${MAX_SOURCE_CHUNKS} bounded batches`,
		{ releaseId, limit: MAX_SOURCE_CHUNKS },
	);
}

async function persistReleaseFilesBatched(
	database: RawAccess,
	releaseId: string,
	files: Record<string, Uint8Array>,
): Promise<void> {
	for (const [path, content] of Object.entries(files)) {
		const chunkCount = Math.max(
			1,
			Math.ceil(content.byteLength / SOURCE_CHUNK_BYTES),
		);
		for (let chunkIndex = 0; chunkIndex < chunkCount; chunkIndex += 1) {
			const offset = chunkIndex * SOURCE_CHUNK_BYTES;
			const chunk = content.slice(offset, offset + SOURCE_CHUNK_BYTES);
			await database.execute(
				`INSERT INTO agentos_apps_release_files
					(release_id, path, chunk_index, content, byte_length)
				 VALUES (?, ?, ?, ?, ?)`,
				releaseId,
				path,
				chunkIndex,
				chunk,
				chunk.byteLength,
			);
		}
	}
}

async function deleteArtifactChunksBatched(
	database: RawAccess,
	releaseId: string,
): Promise<void> {
	for (let batch = 0; batch < MAX_ARTIFACT_CHUNKS; batch += 1) {
		const rows = await database.execute<{ chunks: number }>(
			`SELECT COUNT(*) AS chunks
			 FROM agentos_apps_artifact_chunks
			 WHERE release_id = ?`,
			releaseId,
		);
		if (Number(rows[0]?.chunks ?? 0) === 0) return;
		await database.execute(
			`DELETE FROM agentos_apps_artifact_chunks
			 WHERE rowid IN (
				SELECT rowid FROM agentos_apps_artifact_chunks
				WHERE release_id = ?
				ORDER BY chunk_index
				LIMIT ${ARTIFACT_CHUNKS_PER_TRANSACTION}
			 )`,
			releaseId,
		);
	}
	fail(
		"agentos_apps_artifact_cleanup_limit",
		`artifact cleanup exceeded ${MAX_ARTIFACT_CHUNKS} bounded batches`,
		{ releaseId, limit: MAX_ARTIFACT_CHUNKS },
	);
}

interface ReleaseRow extends Record<string, unknown> {
	release_id: string;
	created_at: number;
	status: StoredAppRelease["status"];
	entrypoint: string;
	artifact_hash: string;
	artifact_bytes: number;
	build_error: string | null;
	regions_json: string;
	scaling_json: string;
	namespace: string;
	envoy_version: number;
	runtime_endpoint: string;
	runtime_pool: string;
	callback_secret?: string;
	uses_rivetkit?: number;
}

function releaseFromRow(row: ReleaseRow): StoredAppRelease {
	return {
		release: row.release_id,
		createdAt: Number(row.created_at),
		status: row.status,
		entrypoint: row.entrypoint,
		artifactHash: row.artifact_hash,
		artifactBytes: Number(row.artifact_bytes),
		error: row.build_error ?? undefined,
		regions: JSON.parse(row.regions_json) as string[],
		scaling: JSON.parse(row.scaling_json) as Required<AppScaling>,
		namespace: row.namespace,
		envoyVersion: Number(row.envoy_version),
		runtimeEndpoint: row.runtime_endpoint,
		runtimePool: row.runtime_pool,
		callbackSecret: row.callback_secret ?? "",
		usesRivetKit: Number(row.uses_rivetkit ?? 0) === 1,
	};
}

async function getStoredRelease(
	database: RawAccess,
	releaseId: string,
): Promise<StoredAppRelease | undefined> {
	const rows = await database.execute<ReleaseRow>(
		"SELECT * FROM agentos_apps_releases WHERE release_id = ?",
		releaseId,
	);
	return rows[0] ? releaseFromRow(rows[0]) : undefined;
}

async function listStoredReleases(
	database: RawAccess,
): Promise<StoredAppRelease[]> {
	const rows = await database.execute<ReleaseRow>(
		"SELECT * FROM agentos_apps_releases ORDER BY created_at ASC",
	);
	return rows.map(releaseFromRow);
}

interface BuildPlan {
	entrypoint: string;
	build: boolean;
	staticRoot?: string;
	dependencyCount: number;
	hasLockfile: boolean;
	usesRivetKit: boolean;
}

function textFile(
	files: Record<string, Uint8Array>,
	path: string,
): string | undefined {
	const content = files[path];
	return content ? new TextDecoder().decode(content) : undefined;
}

function packageExport(value: unknown): string | undefined {
	if (typeof value === "string") return value;
	if (!value || typeof value !== "object" || Array.isArray(value))
		return undefined;
	const object = value as Record<string, unknown>;
	return (
		packageExport(object["."]) ??
		packageExport(object.import) ??
		packageExport(object.default)
	);
}

function validateDeployment(
	input: PreparedDeployAppInput,
	limits: { maxSourceBytes: number; maxFiles: number; maxDependencies: number },
): BuildPlan {
	if (!input || typeof input !== "object" || !input.files) {
		fail(
			"agentos_apps_invalid_files",
			"deployApp files must contain the complete application tree",
		);
	}
	const files = Object.entries(input.files);
	if (files.length === 0 || files.length > limits.maxFiles) {
		fail(
			"agentos_apps_file_count_limit",
			`deployment must contain between 1 and ${limits.maxFiles} files; reduce the source tree`,
			{ observed: files.length, limit: limits.maxFiles },
		);
	}
	let sourceBytes = 0;
	const normalizedFiles: Record<string, Uint8Array> = {};
	for (const [path, content] of files) {
		const normalizedPath = normalizeAppPath(path);
		if (normalizedFiles[normalizedPath]) {
			fail(
				"agentos_apps_duplicate_file_path",
				`multiple deployment paths normalize to ${normalizedPath}`,
			);
		}
		if (!(content instanceof Uint8Array)) {
			fail(
				"agentos_apps_invalid_file",
				`deployment file ${path} must be a string or Uint8Array`,
			);
		}
		normalizedFiles[normalizedPath] = content;
		sourceBytes += content.byteLength;
	}
	if (sourceBytes > limits.maxSourceBytes) {
		fail(
			"agentos_apps_source_limit",
			`deployment source is ${sourceBytes} bytes, exceeding maxSourceBytes ${limits.maxSourceBytes}; reduce the source tree`,
			{ observed: sourceBytes, limit: limits.maxSourceBytes },
		);
	}
	input.files = normalizedFiles;

	const packageJsonSource = textFile(normalizedFiles, "package.json");
	if (!packageJsonSource) {
		if (!normalizedFiles["index.html"]) {
			fail(
				"agentos_apps_entrypoint_not_found",
				"application without package.json must contain index.html",
			);
		}
		return {
			entrypoint: "runner.mjs",
			build: false,
			staticRoot: ".",
			dependencyCount: 0,
			hasLockfile: false,
			usesRivetKit: false,
		};
	}

	let packageJson: {
		dependencies?: unknown;
		devDependencies?: unknown;
		scripts?: { build?: unknown };
		exports?: unknown;
		main?: unknown;
	};
	try {
		packageJson = JSON.parse(packageJsonSource);
	} catch (error) {
		fail(
			"agentos_apps_invalid_package_json",
			"package.json is not valid JSON",
			{
				error: String(error),
			},
		);
	}
	const dependencyCount = [
		packageJson.dependencies,
		packageJson.devDependencies,
	]
		.filter(
			(value): value is Record<string, unknown> =>
				typeof value === "object" && value !== null && !Array.isArray(value),
		)
		.reduce(
			(count, dependencies) => count + Object.keys(dependencies).length,
			0,
		);
	const usesRivetKit = [packageJson.dependencies, packageJson.devDependencies]
		.filter(
			(value): value is Record<string, unknown> =>
				typeof value === "object" && value !== null && !Array.isArray(value),
		)
		.some((dependencies) => typeof dependencies.rivetkit === "string");
	if (dependencyCount > limits.maxDependencies) {
		fail(
			"agentos_apps_dependency_limit",
			`deployment has ${dependencyCount} dependencies, exceeding maxDependencies ${limits.maxDependencies}; reduce dependencies`,
			{ observed: dependencyCount, limit: limits.maxDependencies },
		);
	}
	const build = typeof packageJson.scripts?.build === "string";
	const declaredEntrypoint =
		packageExport(packageJson.exports) ??
		(typeof packageJson.main === "string" ? packageJson.main : undefined);
	if (declaredEntrypoint) {
		return {
			entrypoint: normalizeAppPath(declaredEntrypoint),
			build,
			dependencyCount,
			hasLockfile: Boolean(normalizedFiles["package-lock.json"]),
			usesRivetKit,
		};
	}
	for (const candidate of [
		"src/index.mjs",
		"src/index.js",
		"index.mjs",
		"index.js",
	]) {
		if (normalizedFiles[candidate]) {
			return {
				entrypoint: candidate,
				build,
				dependencyCount,
				hasLockfile: Boolean(normalizedFiles["package-lock.json"]),
				usesRivetKit,
			};
		}
	}
	if (build) {
		return {
			entrypoint: "runner.mjs",
			build: true,
			staticRoot: "dist",
			dependencyCount,
			hasLockfile: Boolean(normalizedFiles["package-lock.json"]),
			usesRivetKit,
		};
	}
	if (normalizedFiles["index.html"]) {
		return {
			entrypoint: "runner.mjs",
			build: false,
			staticRoot: ".",
			dependencyCount,
			hasLockfile: Boolean(normalizedFiles["package-lock.json"]),
			usesRivetKit,
		};
	}
	fail(
		"agentos_apps_entrypoint_not_found",
		"could not infer a server entrypoint or static index.html",
	);
}

function normalizeRegions(
	regions: string[] | undefined,
	fallbackRegion: string,
	maxRegions: number,
): string[] {
	const unique = [...new Set(regions ?? [fallbackRegion || "default"])];
	if (unique.length === 0 || unique.length > maxRegions) {
		fail(
			"agentos_apps_invalid_regions",
			`an app must have between 1 and ${maxRegions} regions; raise maxRegions to allow more`,
			{ maxRegions },
		);
	}
	for (const region of unique) {
		if (!/^[a-z0-9][a-z0-9-]{0,62}$/.test(region)) {
			fail(
				"agentos_apps_invalid_region",
				`invalid region ${JSON.stringify(region)}; use a lowercase Rivet region slug`,
			);
		}
	}
	return unique;
}

function scalerKey(appId: string, release: string, region: string): string[] {
	return [appId, release, region];
}

function replicaKey(
	appId: string,
	release: string,
	region: string,
	index: number,
): string[] {
	return [appId, release, region, String(index)];
}

function boundedOutput(value: string, maximum: number): string {
	const bytes = Buffer.from(value);
	if (bytes.byteLength <= maximum) return value;
	return `${bytes.subarray(0, maximum).toString("utf8")}\n[truncated at ${maximum} bytes]`;
}

const HOP_BY_HOP_HEADERS = [
	"connection",
	"keep-alive",
	"proxy-authenticate",
	"proxy-authorization",
	"te",
	"trailer",
	"transfer-encoding",
	"upgrade",
] as const;

function stripHopByHopHeaders(headers: Headers): void {
	const connectionTokens = (headers.get("connection") ?? "")
		.split(",")
		.map((value) => value.trim().toLowerCase())
		.filter(Boolean);
	for (const name of [...HOP_BY_HOP_HEADERS, ...connectionTokens]) {
		headers.delete(name);
	}
}

function throwCommandFailure(
	kind: "install" | "build" | "pack",
	command: string,
	result: ExecResult,
	maxOutputBytes: number,
): never {
	fail(
		`agentos_apps_${kind}_failed`,
		`${command} failed with exit code ${result.exitCode}`,
		{
			exitCode: result.exitCode,
			stdout: boundedOutput(result.stdout, maxOutputBytes),
			stderr: boundedOutput(result.stderr, maxOutputBytes),
		},
	);
}

async function buildRelease(
	c: AnyActorContext,
	input: PreparedDeployAppInput,
	plan: BuildPlan,
	release: string,
	config: {
		createBuildVm: () => Promise<BuildHandle>;
		buildTimeoutMs: number;
		maxRequestBytes: number;
		maxResponseBytes: number;
		maxBuildOutputBytes: number;
		maxBuildArtifactBytes: number;
		artifactCache?: {
			get(release: string): Promise<Uint8Array | undefined>;
			put(release: string, artifact: Uint8Array): Promise<void>;
		};
	},
): Promise<{ hash: string; size: number; bytes: Uint8Array }> {
	const cached = await config.artifactCache?.get(release);
	if (cached) {
		if (cached.byteLength > config.maxBuildArtifactBytes) {
			fail(
				"agentos_apps_build_artifact_size_limit",
				`cached application artifact is ${cached.byteLength} bytes, limit is maxBuildArtifactBytes ${config.maxBuildArtifactBytes}`,
				{
					artifactBytes: cached.byteLength,
					maxBuildArtifactBytes: config.maxBuildArtifactBytes,
				},
			);
		}
		return {
			hash: createHash("sha256").update(cached).digest("hex"),
			size: cached.byteLength,
			bytes: cached,
		};
	}
	const build = await config.createBuildVm();
	const buildStartedAt = Date.now();
	const logBuildPhase = (phase: string): void => {
		c.log.info({
			msg: "AgentOS Apps build phase completed",
			release,
			phase,
			elapsedMs: Date.now() - buildStartedAt,
		});
	};
	let buildError: unknown;
	try {
		const files: Array<{
			path: string;
			content: string | Uint8Array;
		}> = Object.entries(input.files).map(([path, content]) => ({
			path: `/workspace/${normalizeAppPath(path)}`,
			content,
		}));
		files.push({
			path: "/workspace/runner.mjs",
			content: plan.staticRoot
				? staticRunnerSource({
						root: "public",
						release,
						port: APP_PORT,
					})
				: runnerSource({
						entrypoint: plan.entrypoint,
						release,
						port: APP_PORT,
						maxRequestBytes: config.maxRequestBytes,
						maxResponseBytes: config.maxResponseBytes,
						usesRivetKit: plan.usesRivetKit,
					}),
		});
		const writes = await build.writeFiles(files);
		const failedWrite = writes.find((entry) => !entry.success);
		if (failedWrite) {
			fail(
				"agentos_apps_build_write_failed",
				`failed to write build input ${failedWrite.path}: ${failedWrite.error ?? "unknown error"}`,
				{ path: failedWrite.path, error: failedWrite.error },
			);
		}

		if (input.files["package.json"]) {
			if (plan.usesRivetKit) {
				const packageJson = JSON.parse(
					new TextDecoder().decode(input.files["package.json"]),
				) as Record<string, unknown>;
				const overrides =
					typeof packageJson.overrides === "object" &&
					packageJson.overrides !== null &&
					!Array.isArray(packageJson.overrides)
						? (packageJson.overrides as Record<string, unknown>)
						: {};
				const dependencies =
					typeof packageJson.dependencies === "object" &&
					packageJson.dependencies !== null &&
					!Array.isArray(packageJson.dependencies)
						? (packageJson.dependencies as Record<string, unknown>)
						: {};
				const devDependencies =
					typeof packageJson.devDependencies === "object" &&
					packageJson.devDependencies !== null &&
					!Array.isArray(packageJson.devDependencies)
						? (packageJson.devDependencies as Record<string, unknown>)
						: {};
				const rivetKitVersion =
					typeof dependencies.rivetkit === "string"
						? dependencies.rivetkit
						: typeof devDependencies.rivetkit === "string"
							? devDependencies.rivetkit
							: undefined;
				if (!rivetKitVersion) {
					fail(
						"agentos_apps_invalid_rivetkit_dependency",
						"RivetKit applications must declare a string rivetkit dependency",
					);
				}
				const rivetKitBuildOnlyPackages = [
					"@rivet-dev/agent-os-core",
					"@rivetkit/engine-cli",
					"@rivetkit/rivetkit-napi",
				] as const;
				const needsCompatibilityOverrides = rivetKitBuildOnlyPackages.some(
					(name) => overrides[name] === undefined,
				);
				const needsRuntimeDependencies =
					dependencies.rivetkit === undefined ||
					dependencies["@rivetkit/rivetkit-wasm"] === undefined;
				if (needsCompatibilityOverrides || needsRuntimeDependencies) {
					// RivetKit publishes host integrations as hard dependencies even
					// though its normal serverless/WASM entrypoint does not load them.
					// Avoid recursively packaging AgentOS, the engine binary, and a
					// native addon that cannot execute inside the guest VM.
					for (const name of rivetKitBuildOnlyPackages) {
						overrides[name] ??= "npm:empty-npm-package@1.0.0";
					}
					dependencies.rivetkit ??= rivetKitVersion;
					dependencies["@rivetkit/rivetkit-wasm"] ??= rivetKitVersion;
					packageJson.overrides = overrides;
					packageJson.dependencies = dependencies;
					const compatibilityWrites = await build.writeFiles([
						{
							path: "/workspace/package.json",
							content: JSON.stringify(packageJson),
						},
					]);
					const failedCompatibilityWrite = compatibilityWrites.find(
						(entry) => !entry.success,
					);
					if (failedCompatibilityWrite) {
						fail(
							"agentos_apps_build_write_failed",
							`failed to prepare RivetKit dependency ${failedCompatibilityWrite.path}: ${failedCompatibilityWrite.error ?? "unknown error"}`,
							{
								path: failedCompatibilityWrite.path,
								error: failedCompatibilityWrite.error,
							},
						);
					}

					if (plan.hasLockfile) {
						const reconcileArgs = [
							"install",
							"--package-lock-only",
							"--ignore-scripts",
							"--include=dev",
							"--omit=optional",
							"--omit=peer",
							"--legacy-peer-deps",
							"--no-audit",
							"--no-fund",
							"--maxsockets=16",
							"--loglevel=error",
						];
						const reconcile = await build.execArgv("npm", reconcileArgs, {
							cwd: "/workspace",
							env: {
								NODE_ENV: "development",
								NPM_CONFIG_PRODUCTION: "false",
							},
							timeout: config.buildTimeoutMs,
							captureStdio: true,
						});
						if (reconcile.exitCode !== 0) {
							throwCommandFailure(
								"install",
								"npm install --package-lock-only",
								reconcile,
								config.maxBuildOutputBytes,
							);
						}
					}
				}
			}
			const installArgs = [
				plan.hasLockfile ? "ci" : "install",
				"--install-strategy=shallow",
				"--include=dev",
				"--omit=optional",
				"--omit=peer",
				"--legacy-peer-deps",
				"--no-audit",
				"--no-fund",
				"--maxsockets=16",
				"--loglevel=error",
			];
			const install = await build.execArgv("npm", installArgs, {
				cwd: "/workspace",
				env: {
					NODE_ENV: "development",
					NPM_CONFIG_PRODUCTION: "false",
				},
				timeout: config.buildTimeoutMs,
				captureStdio: true,
			});
			if (install.exitCode !== 0) {
				const debugLog = await build.execArgv(
					"node",
					[
						"-e",
						'const fs=require("node:fs"); const path=require("node:path"); const cache=process.env.npm_config_cache || path.join(process.env.HOME || "/root", ".npm"); const logs=path.join(cache, "_logs"); if(fs.existsSync(logs)){const files=fs.readdirSync(logs).filter((name)=>name.endsWith("-debug-0.log")).sort(); const latest=files[files.length-1]; if(latest) process.stdout.write(fs.readFileSync(path.join(logs, latest), "utf8").slice(-65536));}',
					],
					{
						cwd: "/workspace",
						timeout: 5_000,
						captureStdio: true,
					},
				);
				if (debugLog.exitCode === 0 && debugLog.stdout) {
					install.stderr = `${install.stderr}\n--- npm debug log ---\n${debugLog.stdout}`;
				} else if (debugLog.exitCode !== 0) {
					c.log.error({
						msg: "failed to collect npm debug log",
						exitCode: debugLog.exitCode,
						stderr: boundedOutput(debugLog.stderr, config.maxBuildOutputBytes),
					});
				}
				throwCommandFailure(
					"install",
					`npm ${installArgs[0]}`,
					install,
					config.maxBuildOutputBytes,
				);
			}
			logBuildPhase("dependencies_installed");

			if (plan.build) {
				const result = await build.execArgv("npm", ["run", "build"], {
					cwd: "/workspace",
					timeout: config.buildTimeoutMs,
					captureStdio: true,
				});
				if (result.exitCode !== 0) {
					throwCommandFailure(
						"build",
						"npm run build",
						result,
						config.maxBuildOutputBytes,
					);
				}
				logBuildPhase("application_built");
			}

			const prune = await build.execArgv(
				"npm",
				[
					"prune",
					"--omit=dev",
					"--omit=optional",
					"--omit=peer",
					"--legacy-peer-deps",
				],
				{
					cwd: "/workspace",
					timeout: config.buildTimeoutMs,
					captureStdio: true,
				},
			);
			if (prune.exitCode !== 0) {
				throwCommandFailure(
					"install",
					"npm prune --omit=dev --omit=optional",
					prune,
					config.maxBuildOutputBytes,
				);
			}

			const nativeAddonCheck = await build.execArgv(
				"node",
				[
					"-e",
					'const fs=require("node:fs"); const path=require("node:path"); const found=[]; const walk=(p)=>{if(!fs.existsSync(p))return; for(const e of fs.readdirSync(p,{withFileTypes:true})){const q=path.join(p,e.name); if(e.isDirectory())walk(q); else if(e.name.endsWith(".node"))found.push(q)}}; walk("node_modules"); if(found.length){console.error(found.slice(0,32).join("\\n")); process.exit(42)}',
				],
				{
					cwd: "/workspace",
					timeout: config.buildTimeoutMs,
					captureStdio: true,
				},
			);
			if (nativeAddonCheck.exitCode === 42) {
				fail(
					"agentos_apps_native_addon_unsupported",
					"application contains native Node addons that the AgentOS JavaScript runtime cannot load",
					{
						files: boundedOutput(
							nativeAddonCheck.stderr,
							config.maxBuildOutputBytes,
						),
					},
				);
			}
			if (nativeAddonCheck.exitCode !== 0) {
				throwCommandFailure(
					"build",
					"native addon scan",
					nativeAddonCheck,
					config.maxBuildOutputBytes,
				);
			}
		}

		const bundleConfigPath = "/workspace/.agentos-app-build.json";
		const bundleConfigWrites = await build.writeFiles([
			{
				path: bundleConfigPath,
				content: JSON.stringify({
					version: release,
					workspace: "/workspace",
					release: "/release",
					entrypoint: "runner.mjs",
					staticRoot: plan.staticRoot,
					sourceFiles: Object.keys(input.files),
					usesRivetKit: plan.usesRivetKit,
					maxOutputBytes: config.maxBuildArtifactBytes,
					maxOutputFiles: DEFAULT_MAX_BUILD_ARTIFACT_FILES,
					maxFileBytes: DEFAULT_MAX_BUILD_ARTIFACT_FILE_BYTES,
				}),
			},
		]);
		const failedBundleConfigWrite = bundleConfigWrites.find(
			(entry) => !entry.success,
		);
		if (failedBundleConfigWrite) {
			fail(
				"agentos_apps_build_write_failed",
				`failed to write Apps builder input ${failedBundleConfigWrite.path}: ${failedBundleConfigWrite.error ?? "unknown error"}`,
				{
					path: failedBundleConfigWrite.path,
					error: failedBundleConfigWrite.error,
				},
			);
		}
		const bundle = await build.execArgv(
			"node",
			["/opt/agentos/bin/apps-builder", bundleConfigPath],
			{
				cwd: "/workspace",
				timeout: config.buildTimeoutMs,
				captureStdio: true,
			},
		);
		if (bundle.exitCode !== 0) {
			throwCommandFailure(
				"build",
				"apps-builder",
				bundle,
				config.maxBuildOutputBytes,
			);
		}
		logBuildPhase("release_bundled");

		const packArgs = [
			"--sort=name",
			"--mtime=@0",
			"--owner=0",
			"--group=0",
			"--numeric-owner",
			"-cf",
			build.artifactGuestPath,
			".",
		];
		const pack = await build.execArgv("tar", packArgs, {
			cwd: "/release",
			timeout: config.buildTimeoutMs,
			captureStdio: true,
		});
		if (pack.exitCode !== 0) {
			throwCommandFailure("pack", "tar", pack, config.maxBuildOutputBytes);
		}
		logBuildPhase("release_archived");

		const archiveSize = await build.artifactSize();
		if (
			!Number.isSafeInteger(archiveSize) ||
			archiveSize < 0 ||
			archiveSize > config.maxBuildArtifactBytes
		) {
			fail(
				"agentos_apps_build_artifact_size_limit",
				`built application archive is ${archiveSize} bytes, limit is maxBuildArtifactBytes ${config.maxBuildArtifactBytes}; raise maxBuildArtifactBytes or reduce deployment dependencies`,
				{
					artifactBytes: archiveSize,
					maxBuildArtifactBytes: config.maxBuildArtifactBytes,
				},
			);
		}
		const sourceTar = Buffer.from(await build.readArtifact());
		if (sourceTar.byteLength !== archiveSize) {
			fail(
				"agentos_apps_build_artifact_truncated",
				`build artifact contained ${sourceTar.byteLength} bytes, expected ${archiveSize}`,
				{ expectedBytes: archiveSize, actualBytes: sourceTar.byteLength },
			);
		}
		const packed = packAospkgFromTarBytes(sourceTar).bytes;
		const artifactHash = createHash("sha256").update(packed).digest("hex");
		await config.artifactCache?.put(release, new Uint8Array(packed));
		return {
			hash: artifactHash,
			size: packed.byteLength,
			bytes: new Uint8Array(packed),
		};
	} catch (error) {
		buildError = error;
		throw error;
	} finally {
		await build.dispose().catch((disposeError) => {
			if (!buildError) throw disposeError;
			c.log.error({
				msg: "failed to dispose AgentOS Apps build VM after build failure",
				disposeError,
			});
		});
	}
}

function parseReadyRelease(response: VmFetchResponse): string | null {
	if (response.status !== 200) return null;
	try {
		const value = JSON.parse(new TextDecoder().decode(response.body)) as {
			release?: unknown;
		};
		return typeof value.release === "string" ? value.release : null;
	} catch {
		return null;
	}
}

async function probeReplica(handle: ReplicaHandle): Promise<string | null> {
	try {
		return parseReadyRelease(
			await handle.vmFetch(APP_PORT, "http://agentos-app/.agentos/ready"),
		);
	} catch {
		return null;
	}
}

export function normalizeServerlessCallbackPath(
	request: Request,
): "/api/rivet/metadata" | "/api/rivet/start" | undefined {
	if (!request.headers.get("user-agent")?.startsWith("RivetEngine/")) {
		return undefined;
	}
	const pathname = new URL(request.url).pathname;
	if (request.method === "GET" && pathname.endsWith("/metadata")) {
		return "/api/rivet/metadata";
	}
	if (
		(request.method === "GET" || request.method === "POST") &&
		pathname.endsWith("/start")
	) {
		return "/api/rivet/start";
	}
	return undefined;
}

function validCallbackSecret(request: Request, expected: string): boolean {
	const received = request.headers.get(APP_CALLBACK_SECRET_HEADER);
	if (!received || !expected) return false;
	return timingSafeEqual(
		createHash("sha256").update(received).digest(),
		createHash("sha256").update(expected).digest(),
	);
}

export function resolveAppCallbackSecret(
	releases: ReadonlyArray<{ callbackSecret: string }>,
	activeRelease?: { callbackSecret: string },
): string {
	return (
		activeRelease?.callbackSecret ||
		releases.find((release) => release.callbackSecret)?.callbackSecret ||
		randomUUID()
	);
}

/**
 * Defines the stable app, regional scaler, build VM, and execution-replica
 * actors. Register the returned definitions in one RivetKit registry.
 */
export function createAppsActors(
	options: {
		/** Internal development hook; setupApps() intentionally does not expose this. */
		artifactCache?: {
			get(release: string): Promise<Uint8Array | undefined>;
			put(release: string, artifact: Uint8Array): Promise<void>;
		};
	} = {},
) {
	const maxSourceBytes = DEFAULT_MAX_SOURCE_BYTES;
	const maxFiles = DEFAULT_MAX_FILES;
	const maxVersions = DEFAULT_MAX_VERSIONS;
	const maxRegions = DEFAULT_MAX_REGIONS;
	const maxDependencies = DEFAULT_MAX_DEPENDENCIES;
	const buildTimeoutMs = DEFAULT_BUILD_TIMEOUT_MS;
	const warmTimeoutMs = DEFAULT_WARM_TIMEOUT_MS;
	const warmIdleTimeoutMs = DEFAULT_WARM_IDLE_TIMEOUT_MS;
	const admissionLeaseMs = DEFAULT_ADMISSION_LEASE_MS;
	const maxAdmissions = DEFAULT_MAX_ADMISSIONS;
	const maxRequestBytes = DEFAULT_MAX_REQUEST_BYTES;
	const maxResponseBytes = DEFAULT_MAX_RESPONSE_BYTES;
	const maxBuildArtifactBytes = DEFAULT_MAX_BUILD_ARTIFACT_BYTES;

	async function forwardAppRequest(
		c: AnyActorContext,
		request: Request,
	): Promise<Response> {
		const state = c.state as AppState;
		const release = state.activeRelease
			? await getStoredRelease(c.db, state.activeRelease)
			: undefined;
		if (!release || release.status !== "ready") {
			return new Response("AgentOS App has no active release", { status: 503 });
		}
		const serverlessCallbackPath = normalizeServerlessCallbackPath(request);
		if (
			serverlessCallbackPath &&
			!validCallbackSecret(request, release.callbackSecret)
		) {
			return new Response("Unauthorized", { status: 401 });
		}
		if (
			serverlessCallbackPath === "/api/rivet/metadata" &&
			state.serverlessMetadata?.release === release.release
		) {
			return new Response(
				Buffer.from(state.serverlessMetadata.bodyBase64, "base64"),
				{
					status: state.serverlessMetadata.status,
					statusText: state.serverlessMetadata.statusText,
					headers: state.serverlessMetadata.headers,
				},
			);
		}

		const requestedRegion = request.headers.get("x-agentos-app-region");
		if (requestedRegion && !release.regions.includes(requestedRegion)) {
			return new Response(
				`AgentOS App is not deployed in requested region ${requestedRegion}`,
				{ status: 421 },
			);
		}
		const region = requestedRegion ?? release.regions[0];
		if (!region)
			return new Response("AgentOS App has no configured region", {
				status: 503,
			});

		const contentLength = Number(request.headers.get("content-length") ?? 0);
		if (Number.isFinite(contentLength) && contentLength > maxRequestBytes) {
			return new Response("Request body exceeds AgentOS Apps limit", {
				status: 413,
			});
		}
		const body = await readBoundedRequestBody(request, maxRequestBytes);
		if (body === null) {
			return new Response("Request body exceeds AgentOS Apps limit", {
				status: 413,
			});
		}

		const scaler = c
			.client()
			[SCALER_ACTOR_NAME].getOrCreate(
				scalerKey(c.key[0]!, release.release, region),
				{ createInRegion: region },
			);
		const admission = (await scaler.acquire()) as ReplicaAdmission;
		const replica = c
			.client()
			[REPLICA_ACTOR_NAME].getOrCreate(admission.key) as ReplicaHandle;
		let released = false;
		let renewalTimer: ReturnType<typeof setTimeout> | undefined;
		const scheduleRenewal = () => {
			renewalTimer = setTimeout(
				() => {
					void scaler
						.renew(admission.admissionId)
						.then(scheduleRenewal)
						.catch((error: unknown) => {
							c.log.error({
								msg: "failed to renew app admission lease",
								error,
							});
						});
				},
				Math.max(1_000, Math.floor(admission.leaseMs / 3)),
			);
		};
		scheduleRenewal();
		const releaseAdmission = async () => {
			if (released) return;
			released = true;
			if (renewalTimer) clearTimeout(renewalTimer);
			try {
				await scaler.release(admission.admissionId);
			} catch (error) {
				c.log.error({
					msg: "failed to release app admission; lease expiry will recover it",
					admissionId: admission.admissionId,
					error,
				});
			}
		};

		try {
			const forwardedHeaders = new Headers(request.headers);
			forwardedHeaders.delete("x-agentos-app-region");
			forwardedHeaders.delete(APP_CALLBACK_SECRET_HEADER);
			forwardedHeaders.delete("x-rivet-token");
			if (serverlessCallbackPath) forwardedHeaders.delete("authorization");
			stripHopByHopHeaders(forwardedHeaders);
			const headers: Record<string, string> = {};
			forwardedHeaders.forEach((value, name) => {
				headers[name] = value;
			});
			const forwardedUrl = new URL(request.url);
			forwardedUrl.pathname =
				serverlessCallbackPath ??
				`/${forwardedUrl.pathname.replace(/^\/+/, "")}`;
			if (serverlessCallbackPath) {
				const guestResponse = await replica.fetch(forwardedUrl, {
					method: request.method,
					headers,
					body: body ? Buffer.from(body) : undefined,
				});
				const responseHeaders = new Headers(guestResponse.headers);
				stripHopByHopHeaders(responseHeaders);
				if (serverlessCallbackPath === "/api/rivet/metadata") {
					const responseBody = await readBoundedResponseBody(
						guestResponse,
						DEFAULT_MAX_SERVERLESS_METADATA_BYTES,
					);
					await releaseAdmission();
					if (guestResponse.status < 200 || guestResponse.status >= 300) {
						c.log.warn({
							msg: "AgentOS Apps guest metadata request failed",
							status: guestResponse.status,
							path: forwardedUrl.pathname,
							body: new TextDecoder().decode(responseBody).slice(0, 2_048),
						});
					} else {
						state.serverlessMetadata = {
							release: release.release,
							status: guestResponse.status,
							statusText: guestResponse.statusText,
							headers: Object.fromEntries(responseHeaders),
							bodyBase64: Buffer.from(responseBody).toString("base64"),
						};
					}
					return new Response(Buffer.from(responseBody), {
						status: guestResponse.status,
						statusText: guestResponse.statusText,
						headers: responseHeaders,
					});
				}
				const reader = guestResponse.body?.getReader();
				const responseBody = reader
					? new ReadableStream<Uint8Array>({
							async pull(controller) {
								try {
									const chunk = await reader.read();
									if (chunk.done) {
										controller.close();
										await releaseAdmission();
									} else {
										controller.enqueue(chunk.value);
									}
								} catch (error) {
									controller.error(error);
									await releaseAdmission();
								}
							},
							async cancel(reason) {
								await reader.cancel(reason).catch((error) => {
									c.log.error({
										msg: "failed to cancel guest RivetKit callback stream",
										error,
									});
								});
								await releaseAdmission();
							},
						})
					: null;
				if (!reader) await releaseAdmission();
				return new Response(responseBody, {
					status: guestResponse.status,
					statusText: guestResponse.statusText,
					headers: responseHeaders,
				});
			}
			const guestResponse = await replica.vmFetchStreamStart(
				APP_PORT,
				forwardedUrl.href,
				{
					method: request.method,
					headers,
					body,
				},
			);
			const responseHeaders = new Headers(
				guestResponse.rawHeaders ?? Object.entries(guestResponse.headers),
			);
			stripHopByHopHeaders(responseHeaders);
			if (serverlessCallbackPath === "/api/rivet/metadata") {
				const chunks: Uint8Array[] = [];
				let metadataBytes = 0;
				try {
					for (;;) {
						const chunk = await replica.vmFetchStreamRead(
							guestResponse.streamId,
						);
						metadataBytes += chunk.body.byteLength;
						if (metadataBytes > DEFAULT_MAX_SERVERLESS_METADATA_BYTES) {
							await replica.vmFetchStreamCancel(guestResponse.streamId);
							fail(
								"agentos_apps_metadata_limit",
								`RivetKit metadata exceeds ${DEFAULT_MAX_SERVERLESS_METADATA_BYTES} bytes`,
								{ limit: DEFAULT_MAX_SERVERLESS_METADATA_BYTES },
							);
						}
						if (chunk.body.byteLength > 0) chunks.push(chunk.body);
						if (chunk.done) break;
					}
				} finally {
					await releaseAdmission();
				}
				const metadataBody = new Uint8Array(
					Buffer.concat(chunks, metadataBytes),
				);
				const headers = Object.fromEntries(responseHeaders);
				if (guestResponse.status < 200 || guestResponse.status >= 300) {
					c.log.warn({
						msg: "AgentOS Apps guest metadata request failed",
						status: guestResponse.status,
						path: forwardedUrl.pathname,
						body: new TextDecoder().decode(metadataBody).slice(0, 2_048),
					});
				}
				if (guestResponse.status >= 200 && guestResponse.status < 300) {
					state.serverlessMetadata = {
						release: release.release,
						status: guestResponse.status,
						statusText: guestResponse.statusText,
						headers,
						bodyBase64: Buffer.from(metadataBody).toString("base64"),
					};
				}
				return new Response(metadataBody, {
					status: guestResponse.status,
					statusText: guestResponse.statusText,
					headers,
				});
			}
			const hasResponseBody =
				request.method !== "HEAD" &&
				![101, 204, 205, 304].includes(guestResponse.status);
			let responseBytes = 0;
			const responseBody = hasResponseBody
				? new ReadableStream<Uint8Array>({
						async pull(controller) {
							try {
								const chunk = await replica.vmFetchStreamRead(
									guestResponse.streamId,
								);
								responseBytes += chunk.body.byteLength;
								if (responseBytes > maxResponseBytes) {
									await replica.vmFetchStreamCancel(guestResponse.streamId);
									throw new AgentOSAppsError(
										"agentos_apps_response_limit",
										`response exceeds maxResponseBytes ${maxResponseBytes}; raise maxResponseBytes to allow a larger response`,
										{ maxResponseBytes },
									);
								}
								if (chunk.body.byteLength) controller.enqueue(chunk.body);
								if (chunk.done) {
									await releaseAdmission();
									controller.close();
								}
							} catch (error) {
								await replica
									.vmFetchStreamCancel(guestResponse.streamId)
									.catch((cancelError) => {
										c.log.error({
											msg: "failed to cancel app response stream",
											cancelError,
										});
									});
								await releaseAdmission().catch((releaseError) => {
									c.log.error({
										msg: "failed to release app admission",
										releaseError,
									});
								});
								controller.error(error);
							}
						},
						async cancel() {
							try {
								await replica.vmFetchStreamCancel(guestResponse.streamId);
							} finally {
								await releaseAdmission();
							}
						},
					})
				: null;
			if (!hasResponseBody) {
				await replica.vmFetchStreamCancel(guestResponse.streamId);
				await releaseAdmission();
			}
			responseHeaders.set("x-agentos-app-replica", admission.key.join("/"));
			responseHeaders.set("x-agentos-app-release", admission.release);
			responseHeaders.set(
				"x-agentos-app-replica-count",
				String(admission.replicaCount),
			);
			responseHeaders.set(
				"x-agentos-app-queue-delay-ms",
				String(admission.queueDelayMs),
			);
			responseHeaders.set(
				"x-agentos-app-cold-start",
				admission.coldStart ? "1" : "0",
			);
			return new Response(responseBody, {
				status: guestResponse.status,
				statusText: guestResponse.statusText,
				headers: responseHeaders,
			});
		} catch (error) {
			await releaseAdmission().catch((releaseError) => {
				c.log.error({
					msg: "failed to release app admission",
					releaseError,
				});
			});
			throw error;
		}
	}

	const agentOSAppsApp = actor({
		inspector: {
			tabs: [
				{
					id: "agentos-app",
					label: "AgentOS App",
					icon: "box",
					source: `${INSPECTOR_ROOT}/deployment`,
				},
			],
		},
		options: {
			actionTimeout: buildTimeoutMs + warmTimeoutMs + 60_000,
		},
		db: db({ onMigrate: migrateAppsTables }),
		createState: (): AppState => ({
			activeRelease: null,
			namespace: null,
			revision: 0,
			nextEnvoyVersion: 1,
		}),
		onRequest: forwardAppRequest,
		actions: {
			deploy: async (
				c: AnyActorContext,
				input: PreparedDeployAppInput,
			): Promise<Deployment & { appActorId: string; usesRivetKit: boolean }> =>
				c.keepAwake(
					serialized(`app:${c.actorId}`, async () => {
						const appId = c.key[0];
						if (!appId || c.key.length !== 1 || input.appId !== appId) {
							fail(
								"agentos_apps_app_id_mismatch",
								"deployApp appId must match the stable application actor key",
								{ appId: input.appId, actorKey: c.key },
							);
						}
						const plan = validateDeployment(input, {
							maxSourceBytes,
							maxFiles,
							maxDependencies,
						});
						const regions = normalizeRegions(
							input.regions,
							c.region,
							maxRegions,
						);
						const scaling = normalizeScaling(input.scaling);
						const releaseId = canonicalDeploymentHash({
							files: input.files,
							entrypoint: plan.entrypoint,
							build: plan.build,
							staticRoot: plan.staticRoot,
							packagingIdentity: [
								`apps-builder@${appsBuilderVersion}`,
								`manifest@${appBundleManifestVersion}`,
								"bundle@2",
								"esbuild-wasm@0.27.4",
								"rivetkit-adapter@6",
							].join(";"),
							deploymentIdentity: JSON.stringify({
								regions,
								scaling,
								namespace: input.namespace,
								runtime: input.runtime,
								usesRivetKit: plan.usesRivetKit,
							}),
						});
						const state = c.state as AppState;
						const releasesBefore = await listStoredReleases(c.db);
						state.nextEnvoyVersion ??=
							Math.max(
								0,
								...releasesBefore.map((candidate) => candidate.envoyVersion),
							) + 1;
						const previousReleaseId = state.activeRelease;
						const previousServerlessMetadata = state.serverlessMetadata;
						const previousRelease = previousReleaseId
							? await getStoredRelease(c.db, previousReleaseId)
							: undefined;
						const callbackSecret = resolveAppCallbackSecret(
							releasesBefore,
							previousRelease,
						);
						if (state.namespace && state.namespace !== input.namespace) {
							fail(
								"agentos_apps_namespace_changed",
								"an appId cannot be reassigned to a different Rivet namespace",
								{
									appId,
									expected: state.namespace,
									received: input.namespace,
								},
							);
						}
						state.namespace = input.namespace;
						let release = await getStoredRelease(c.db, releaseId);
						if (release && release.callbackSecret !== callbackSecret) {
							release.callbackSecret = callbackSecret;
							await c.db.execute(
								`UPDATE agentos_apps_releases
										 SET callback_secret = ?
										 WHERE release_id = ?`,
								release.callbackSecret,
								releaseId,
							);
						}
						const previousRegions = [...(release?.regions ?? [])];
						const envoyVersion =
							previousReleaseId === releaseId && release
								? release.envoyVersion
								: state.nextEnvoyVersion++;

						if (!release || release.status !== "ready") {
							release = {
								release: releaseId,
								artifactHash: "",
								artifactBytes: 0,
								createdAt: release?.createdAt ?? Date.now(),
								regions,
								scaling,
								status: "building",
								entrypoint: plan.entrypoint,
								namespace: input.namespace,
								envoyVersion,
								runtimeEndpoint: input.runtime.endpoint,
								runtimePool: input.runtime.pool,
								usesRivetKit: plan.usesRivetKit,
								callbackSecret,
							};
							const releaseCreatedAt = release.createdAt;
							await deleteArtifactChunksBatched(c.db, releaseId);
							await c.db.transaction(async (tx) => {
								await tx.execute(
									`INSERT INTO agentos_apps_releases (
											release_id, created_at, status, entrypoint,
											artifact_hash, artifact_bytes, build_error,
											regions_json, scaling_json, namespace, envoy_version,
											runtime_endpoint, runtime_pool, callback_secret,
											uses_rivetkit
										) VALUES (?, ?, ?, ?, '', 0, NULL, ?, ?, ?, ?, ?, ?, ?, ?)
										ON CONFLICT(release_id) DO UPDATE SET
										status = excluded.status,
										entrypoint = excluded.entrypoint,
										build_error = NULL,
										regions_json = excluded.regions_json,
										scaling_json = excluded.scaling_json,
											namespace = excluded.namespace,
											envoy_version = excluded.envoy_version,
											runtime_endpoint = excluded.runtime_endpoint,
											runtime_pool = excluded.runtime_pool,
											callback_secret = excluded.callback_secret,
											uses_rivetkit = excluded.uses_rivetkit`,
									releaseId,
									releaseCreatedAt,
									"building",
									plan.entrypoint,
									JSON.stringify(regions),
									JSON.stringify(scaling),
									input.namespace,
									envoyVersion,
									input.runtime.endpoint,
									input.runtime.pool,
									callbackSecret,
									plan.usesRivetKit ? 1 : 0,
								);
							});
							await deleteReleaseFilesBatched(c.db, releaseId);
							await persistReleaseFilesBatched(c.db, releaseId, input.files);
							try {
								const artifact = await buildRelease(c, input, plan, releaseId, {
									createBuildVm,
									buildTimeoutMs,
									maxRequestBytes,
									maxResponseBytes,
									maxBuildOutputBytes: DEFAULT_MAX_BUILD_OUTPUT_BYTES,
									maxBuildArtifactBytes,
									artifactCache: options.artifactCache,
								});
								const chunkCount = Math.ceil(
									artifact.size / ARTIFACT_CHUNK_BYTES,
								);
								if (chunkCount > MAX_ARTIFACT_CHUNKS) {
									fail(
										"agentos_apps_artifact_chunk_limit",
										`artifact requires ${chunkCount} chunks, exceeding ${MAX_ARTIFACT_CHUNKS}`,
										{ observed: chunkCount, limit: MAX_ARTIFACT_CHUNKS },
									);
								}
								await deleteArtifactChunksBatched(c.db, releaseId);
								for (
									let firstChunk = 0;
									firstChunk < chunkCount;
									firstChunk += ARTIFACT_CHUNKS_PER_TRANSACTION
								) {
									await c.db.transaction(async (tx) => {
										const endChunk = Math.min(
											chunkCount,
											firstChunk + ARTIFACT_CHUNKS_PER_TRANSACTION,
										);
										for (let index = firstChunk; index < endChunk; index += 1) {
											const offset = index * ARTIFACT_CHUNK_BYTES;
											const content = artifact.bytes.slice(
												offset,
												offset + ARTIFACT_CHUNK_BYTES,
											);
											await tx.execute(
												`INSERT INTO agentos_apps_artifact_chunks
													(release_id, chunk_index, content, byte_length)
												 VALUES (?, ?, ?, ?)`,
												releaseId,
												index,
												content,
												content.byteLength,
											);
										}
									});
								}
								const totals = await c.db.execute<{
									bytes: number;
									chunks: number;
								}>(
									`SELECT COALESCE(SUM(byte_length), 0) AS bytes,
										COUNT(*) AS chunks
									 FROM agentos_apps_artifact_chunks
									 WHERE release_id = ?`,
									releaseId,
								);
								if (
									Number(totals[0]?.bytes ?? 0) !== artifact.size ||
									Number(totals[0]?.chunks ?? 0) !== chunkCount
								) {
									fail(
										"agentos_apps_artifact_persist_mismatch",
										"persisted artifact chunks failed length verification",
										{
											expectedBytes: artifact.size,
											actualBytes: Number(totals[0]?.bytes ?? 0),
										},
									);
								}
								await c.db.execute(
									`UPDATE agentos_apps_releases
									 SET status = 'ready', artifact_hash = ?,
									     artifact_bytes = ?, build_error = NULL
									 WHERE release_id = ?`,
									artifact.hash,
									artifact.size,
									releaseId,
								);
								release.artifactHash = artifact.hash;
								release.artifactBytes = artifact.size;
								release.status = "ready";
								delete release.error;
							} catch (error) {
								release.status = "failed";
								release.error =
									error instanceof Error ? error.message : String(error);
								await deleteArtifactChunksBatched(c.db, releaseId);
								await c.db.execute(
									`UPDATE agentos_apps_releases
									 SET status = 'failed', build_error = ?
									 WHERE release_id = ?`,
									release.error,
									releaseId,
								);
								c.log.error({
									msg: "AgentOS App build failed",
									release: releaseId,
									error,
								});
								throw error;
							}
						} else {
							release = {
								...release,
								regions,
								scaling,
								envoyVersion,
								runtimeEndpoint: input.runtime.endpoint,
								runtimePool: input.runtime.pool,
								usesRivetKit: plan.usesRivetKit,
							};
							await c.db.execute(
								`UPDATE agentos_apps_releases
								 SET regions_json = ?, scaling_json = ?, envoy_version = ?,
								     runtime_endpoint = ?, runtime_pool = ?,
								     uses_rivetkit = ?
								 WHERE release_id = ?`,
								JSON.stringify(regions),
								JSON.stringify(scaling),
								envoyVersion,
								input.runtime.endpoint,
								input.runtime.pool,
								plan.usesRivetKit ? 1 : 0,
								releaseId,
							);
						}

						const rolloutRelease: StoredAppRelease = {
							...release,
							regions,
							scaling,
						};
						const client = c.client();
						const rolloutResults = await Promise.allSettled(
							regions.map((region) =>
								client[SCALER_ACTOR_NAME]
									.getOrCreate(scalerKey(appId, releaseId, region), {
										createInRegion: region,
									})
									.prepare({
										appId,
										release: rolloutRelease,
										region,
										verifyReplica: true,
									}),
							),
						);
						const rolloutFailure = rolloutResults.find(
							(result) => result.status === "rejected",
						);
						if (rolloutFailure?.status === "rejected") {
							const cleanupRegions =
								previousReleaseId === releaseId
									? regions.filter(
											(region) => !previousRegions.includes(region),
										)
									: regions;
							const cleanup = await Promise.allSettled(
								cleanupRegions.map((region) =>
									client[SCALER_ACTOR_NAME]
										.getOrCreate(scalerKey(appId, releaseId, region), {
											createInRegion: region,
										})
										.retire(),
								),
							);
							for (const result of cleanup) {
								if (result.status === "rejected") {
									c.log.error({
										msg: "failed to clean up an unsuccessful AgentOS Apps rollout",
										release: releaseId,
										error: result.reason,
									});
								}
							}
							throw rolloutFailure.reason;
						}
						if (plan.usesRivetKit) {
							const connection = resolveDefaultRivetConnection();
							if (
								connection.endpoint.replace(/\/$/, "") !==
								input.runtime.endpoint.replace(/\/$/, "")
							) {
								fail(
									"agentos_apps_runtime_changed",
									"the app actor Rivet endpoint does not match the deployment runtime",
									{
										expected: connection.endpoint,
										received: input.runtime.endpoint,
									},
								);
							}
							// The Engine starts polling metadata as soon as the runner
							// config is written. Make this healthy release visible for
							// that handshake, then roll back if configuration fails.
							state.activeRelease = releaseId;
							state.serverlessMetadata = undefined;
							try {
								await configureAppNamespaceRunner(
									c.actorId,
									{
										endpoint: input.runtime.endpoint,
										namespace: input.namespace,
										pool: input.runtime.pool,
									},
									release.callbackSecret,
									connection,
								);
							} catch (error) {
								state.activeRelease = previousReleaseId;
								state.serverlessMetadata = previousServerlessMetadata;
								const cleanup = await Promise.allSettled(
									regions.map((region) =>
										client[SCALER_ACTOR_NAME]
											.getOrCreate(scalerKey(appId, releaseId, region), {
												createInRegion: region,
											})
											.retire(),
									),
								);
								for (const result of cleanup) {
									if (result.status === "rejected") {
										c.log.error({
											msg: "failed to clean up an AgentOS Apps rollout after runner configuration failed",
											release: releaseId,
											error: result.reason,
										});
									}
								}
								throw error;
							}
						}
						state.activeRelease = releaseId;
						if (!plan.usesRivetKit) state.serverlessMetadata = undefined;
						state.revision += 1;
						const retiredRegions =
							previousRelease && previousRelease.release !== releaseId
								? previousRelease.regions.map((region) => ({
										release: previousRelease.release,
										region,
									}))
								: previousRegions
										.filter((region) => !regions.includes(region))
										.map((region) => ({ release: releaseId, region }));
						if (retiredRegions.length > 0) {
							const retirements = await Promise.allSettled(
								retiredRegions.map((retirement) =>
									client[SCALER_ACTOR_NAME]
										.getOrCreate(
											scalerKey(appId, retirement.release, retirement.region),
											{ createInRegion: retirement.region },
										)
										.retire(),
								),
							);
							for (const retirement of retirements) {
								if (retirement.status === "rejected") {
									c.log.error({
										msg: "failed to retire an inactive AgentOS Apps release",
										release: releaseId,
										error: retirement.reason,
									});
								}
							}
						}
						const releases = await listStoredReleases(c.db);
						if (releases.length > maxVersions) {
							const removable = releases
								.filter((candidate) => candidate.release !== releaseId)
								.sort((a, b) => a.createdAt - b.createdAt);
							let retained = releases.length;
							while (retained > maxVersions) {
								const candidate = removable.shift();
								if (!candidate) break;
								const retirements = await Promise.allSettled(
									candidate.regions.map((region) =>
										client[SCALER_ACTOR_NAME]
											.getOrCreate(
												scalerKey(appId, candidate.release, region),
												{ createInRegion: region },
											)
											.retire(),
									),
								);
								const stillReferenced = retirements.some(
									(result) =>
										result.status === "rejected" ||
										Number(result.value?.drainingReplicas ?? 0) > 0,
								);
								if (stillReferenced) {
									c.log.warn({
										msg: "deferred AgentOS Apps release garbage collection while replicas are still draining",
										appId,
										release: candidate.release,
									});
									continue;
								}
								await deleteArtifactChunksBatched(c.db, candidate.release);
								await deleteReleaseFilesBatched(c.db, candidate.release);
								await c.db.execute(
									"DELETE FROM agentos_apps_releases WHERE release_id = ?",
									candidate.release,
								);
								retained -= 1;
							}
						}
						return {
							appId,
							release: releaseId,
							namespace: input.namespace,
							pool: input.runtime.pool,
							regions,
							appActorId: c.actorId,
							usesRivetKit: plan.usesRivetKit,
						};
					}),
				),
			resolveDeployment: async (
				c: AnyActorContext,
				requestedRegion?: string,
			) => {
				const state = c.state as AppState;
				const release = state.activeRelease
					? await getStoredRelease(c.db, state.activeRelease)
					: undefined;
				if (!release || release.status !== "ready") {
					fail(
						"agentos_apps_not_deployed",
						"app has no active release; call app.deploy() first",
					);
				}
				if (requestedRegion && !release.regions.includes(requestedRegion)) {
					fail(
						"agentos_apps_region_not_deployed",
						`app is not deployed in requested region ${requestedRegion}`,
						{ requestedRegion, regions: release.regions },
					);
				}
				const region = requestedRegion ?? release.regions[0];
				if (!region) fail("agentos_apps_no_region", "active app has no region");
				return {
					appId: c.key[0],
					release: release.release,
					region,
					scalerKey: scalerKey(c.key[0]!, release.release, region),
					revision: state.revision,
					maxRequestBytes,
					maxResponseBytes,
				};
			},
			getRelease: async (c: AnyActorContext, releaseId: string) => {
				const release = await getStoredRelease(c.db, releaseId);
				if (!release) {
					fail(
						"agentos_apps_release_not_found",
						`app release ${releaseId} was not found`,
					);
				}
				const { callbackSecret: _callbackSecret, ...publicRelease } = release;
				return publicRelease;
			},
			getArtifactManifest: async (c: AnyActorContext, releaseId: string) => {
				const release = await getStoredRelease(c.db, releaseId);
				if (!release || release.status !== "ready") {
					fail(
						"agentos_apps_artifact_not_ready",
						`artifact for release ${releaseId} is not ready`,
					);
				}
				const rows = await c.db.execute<{ chunks: number; bytes: number }>(
					`SELECT COUNT(*) AS chunks,
						COALESCE(SUM(byte_length), 0) AS bytes
					 FROM agentos_apps_artifact_chunks WHERE release_id = ?`,
					releaseId,
				);
				const chunks = Number(rows[0]?.chunks ?? 0);
				const bytes = Number(rows[0]?.bytes ?? 0);
				if (chunks > MAX_ARTIFACT_CHUNKS || bytes !== release.artifactBytes) {
					fail(
						"agentos_apps_artifact_manifest_invalid",
						`artifact ${releaseId} failed persisted manifest validation`,
						{ chunks, bytes, expectedBytes: release.artifactBytes },
					);
				}
				return {
					hash: release.artifactHash,
					bytes,
					chunks,
					chunkBytes: ARTIFACT_CHUNK_BYTES,
				};
			},
			readArtifactChunk: async (
				c: AnyActorContext,
				releaseId: string,
				index: number,
			) => {
				if (
					!Number.isInteger(index) ||
					index < 0 ||
					index >= MAX_ARTIFACT_CHUNKS
				) {
					fail(
						"agentos_apps_invalid_artifact_chunk",
						`artifact chunk index must be between 0 and ${MAX_ARTIFACT_CHUNKS - 1}`,
						{ index },
					);
				}
				const rows = await c.db.execute<{
					content: Uint8Array;
					byte_length: number;
				}>(
					`SELECT content, byte_length
					 FROM agentos_apps_artifact_chunks
					 WHERE release_id = ? AND chunk_index = ?`,
					releaseId,
					index,
				);
				const row = rows[0];
				if (!row) {
					fail(
						"agentos_apps_artifact_chunk_not_found",
						`artifact chunk ${index} for release ${releaseId} was not found`,
					);
				}
				const content = new Uint8Array(row.content);
				if (
					content.byteLength !== Number(row.byte_length) ||
					content.byteLength > ARTIFACT_CHUNK_BYTES
				) {
					fail(
						"agentos_apps_artifact_chunk_invalid",
						`artifact chunk ${index} failed length validation`,
					);
				}
				return content;
			},
			inspect: async (c: AnyActorContext) => {
				const state = c.state as AppState;
				return {
					activeRelease: state.activeRelease,
					namespace: state.namespace,
					revision: state.revision,
					releases: (await listStoredReleases(c.db)).map(
						({
							entrypoint: _entrypoint,
							namespace: _namespace,
							callbackSecret: _callbackSecret,
							...release
						}) => release,
					),
				};
			},
		},
	});

	function updateCapacityWarning(
		c: AnyActorContext,
		state: ReturnType<typeof requireScalerState>,
	): void {
		const provisioned = state.replicas.length + state.warmingReplicas;
		const aboveHalf = provisioned > state.scaling.maxReplicas / 2;
		if (aboveHalf && !state.capacityWarningLatched) {
			state.capacityWarningLatched = true;
			c.log.warn({
				msg: `AgentOS Apps scaler is above 50% of maxReplicas ${state.scaling.maxReplicas}; raise scaling.maxReplicas if this app needs more capacity`,
				appId: state.appId,
				release: state.release,
				region: state.region,
				readyReplicas: state.replicas.length,
				warmingReplicas: state.warmingReplicas,
				maxReplicas: state.scaling.maxReplicas,
				utilizationPercent: (provisioned / state.scaling.maxReplicas) * 100,
			});
		} else if (!aboveHalf && state.capacityWarningLatched) {
			state.capacityWarningLatched = false;
		}
	}

	async function reserveReplica(
		c: AnyActorContext,
	): Promise<{ index: number; key: string[] } | null> {
		return serialized(`scaler:${c.actorId}`, async () => {
			const state = requireScalerState(c);
			if (
				state.replicas.length + state.warmingReplicas >=
				state.scaling.maxReplicas
			) {
				return null;
			}
			const index = state.nextReplicaIndex++;
			const key = replicaKey(state.appId, state.release, state.region, index);
			state.warmingReplicaKeys ??= [];
			state.warmingReplicaKeys.push(key);
			state.warmingReplicas = state.warmingReplicaKeys.length;
			state.revision += 1;
			updateCapacityWarning(c, state);
			return { index, key };
		});
	}

	async function warmReservedReplica(
		c: AnyActorContext,
		reservation: { index: number; key: string[] },
	): Promise<boolean> {
		const initial = requireScalerState(c);
		const { appId, release: releaseId, region } = initial;
		const client = c.client();
		const key = reservation.key;
		let handle: ReplicaHandle | undefined;
		try {
			const release = (await client[APP_ACTOR_NAME]
				.getOrCreate([appId])
				.getRelease(releaseId)) as StoredAppRelease;
			handle = client[REPLICA_ACTOR_NAME].getOrCreate(key, {
				createInRegion: region,
			}) as ReplicaHandle;
			await handle.configure({
				appId,
				release: release.release,
				artifactHash: release.artifactHash,
				artifactBytes: release.artifactBytes,
				namespace: release.namespace,
				envoyVersion: release.envoyVersion,
				usesRivetKit: release.usesRivetKit,
				runtime: {
					namespace: release.namespace,
					endpoint: release.runtimeEndpoint,
					pool: release.runtimePool,
				},
			});
			const inspection = await handle.inspect();
			if ((await probeReplica(handle)) !== release.release) {
				const deadline = Date.now() + warmTimeoutMs;
				await new Promise((resolve) => setTimeout(resolve, 200));
				while (Date.now() < deadline) {
					if ((await probeReplica(handle)) === release.release) break;
					await new Promise((resolve) => setTimeout(resolve, 50));
				}
				if ((await probeReplica(handle)) !== release.release) {
					fail(
						"agentos_apps_replica_warm_timeout",
						`execution replica did not become ready within warmTimeoutMs ${warmTimeoutMs}`,
						{ release: release.release, warmTimeoutMs },
					);
				}
			}
			if (inspection.startedAt === null) await handle.markStarted();
			const registered = await serialized(`scaler:${c.actorId}`, async () => {
				const state = requireScalerState(c);
				state.warmingReplicaKeys = (state.warmingReplicaKeys ?? []).filter(
					(candidate) => candidate.join("\0") !== key.join("\0"),
				);
				state.warmingReplicas = state.warmingReplicaKeys.length;
				if (
					state.retired ||
					state.appId !== appId ||
					state.release !== releaseId ||
					state.region !== region
				) {
					state.revision += 1;
					updateCapacityWarning(c, state);
					return false;
				}
				const now = Date.now();
				state.replicas.push({
					key,
					readyAt: now,
					activeRequests: 0,
					lastUsedAt: now,
					draining: false,
				});
				state.revision += 1;
				updateCapacityWarning(c, state);
				return true;
			});
			if (!registered) {
				await handle.destroy();
				await serialized(`scaler:${c.actorId}`, async () => {
					const state = requireScalerState(c);
					if (
						state.retired &&
						state.replicas.length === 0 &&
						state.warmingReplicas === 0
					) {
						c.destroy();
					}
				});
				return false;
			}
			return true;
		} catch (error) {
			await handle?.destroy().catch((destroyError) => {
				c.log.error({
					msg: "failed to destroy unsuccessful AgentOS Apps replica",
					appId,
					release: releaseId,
					region,
					destroyError,
				});
			});
			await serialized(`scaler:${c.actorId}`, async () => {
				const state = requireScalerState(c);
				state.warmingReplicaKeys = (state.warmingReplicaKeys ?? []).filter(
					(candidate) => candidate.join("\0") !== key.join("\0"),
				);
				state.warmingReplicas = state.warmingReplicaKeys.length;
				state.revision += 1;
				updateCapacityWarning(c, state);
				if (
					state.retired &&
					state.replicas.length === 0 &&
					state.warmingReplicas === 0
				) {
					c.destroy();
				}
			});
			throw error;
		}
	}

	async function addReplica(c: AnyActorContext): Promise<boolean> {
		const reservation = await reserveReplica(c);
		return reservation === null ? false : warmReservedReplica(c, reservation);
	}

	function requireScalerState(c: AnyActorContext): ScalerState & {
		appId: string;
		release: string;
		region: string;
		scaling: Required<AppScaling>;
	} {
		const state = c.state as ScalerState;
		if (!state.appId || !state.release || !state.region || !state.scaling) {
			fail(
				"agentos_apps_scaler_uninitialized",
				"regional scaler has not been prepared by an app deployment",
			);
		}
		state.selectionCursor ??= 0;
		state.nextReplicaIndex ??=
			Math.max(
				-1,
				...state.replicas.map((replica) => Number(replica.key.at(-1) ?? -1)),
			) + 1;
		state.reconcileScheduledAt ??= null;
		state.admissions ??= {};
		state.warmingReplicaKeys ??= [];
		state.warmingReplicas = state.warmingReplicaKeys.length;
		state.capacityWarningLatched ??= false;
		for (const replica of state.replicas) {
			replica.activeRequests ??= 0;
			replica.lastUsedAt ??= replica.readyAt;
			replica.draining ??= false;
		}
		return state as ScalerState & {
			appId: string;
			release: string;
			region: string;
			scaling: Required<AppScaling>;
		};
	}

	function expireAdmissions(
		c: AnyActorContext,
		state: ReturnType<typeof requireScalerState>,
	): number {
		const now = Date.now();
		let expired = 0;
		for (const [id, admission] of Object.entries(state.admissions ?? {})) {
			if (admission.expiresAt > now) continue;
			delete state.admissions?.[id];
			const replica = state.replicas.find(
				(candidate) =>
					candidate.key.join("\0") === admission.replicaKey.join("\0"),
			);
			if (replica) {
				replica.activeRequests = Math.max(0, replica.activeRequests - 1);
				replica.lastUsedAt = now;
			}
			expired += 1;
		}
		if (expired > 0) {
			state.revision += 1;
			c.log.warn({
				msg: "expired abandoned AgentOS Apps admissions",
				expired,
				admissionLeaseMs,
			});
		}
		return expired;
	}

	async function reconcileIdleReplicas(
		c: AnyActorContext,
		state: ReturnType<typeof requireScalerState>,
	): Promise<number> {
		expireAdmissions(c, state);
		let removed = 0;
		const now = Date.now();
		const candidates = state.replicas
			.filter(
				(replica) =>
					replica.activeRequests === 0 &&
					(replica.draining || now - replica.lastUsedAt >= warmIdleTimeoutMs),
			)
			.sort((a, b) => a.lastUsedAt - b.lastUsedAt);
		const maxRemoval = Math.max(1, Math.ceil(state.replicas.length * 0.25));
		while (
			state.replicas.length > state.scaling.minReplicas &&
			candidates.length > 0 &&
			removed < maxRemoval
		) {
			const candidate = candidates.shift();
			if (!candidate) break;
			const index = state.replicas.findIndex(
				(replica) => replica.key.join("\0") === candidate.key.join("\0"),
			);
			if (index >= 0) {
				const handle = c
					.client()
					[REPLICA_ACTOR_NAME].getOrCreate(candidate.key) as ReplicaHandle;
				await handle.destroy();
				state.replicas.splice(index, 1);
				removed += 1;
			}
		}
		if (removed > 0) {
			state.revision += 1;
			updateCapacityWarning(c, state);
		}
		return removed;
	}

	const agentOSAppsScaler = actor({
		inspector: {
			tabs: [
				{
					id: "agentos-scaler",
					label: "Regional scaler",
					icon: "activity",
					source: `${INSPECTOR_ROOT}/scaler`,
				},
			],
		},
		options: {
			actionTimeout: warmTimeoutMs + 30_000,
		},
		createState: (): ScalerState => ({
			appId: null,
			release: null,
			region: null,
			scaling: null,
			replicas: [],
			warmingReplicas: 0,
			warmingReplicaKeys: [],
			capacityWarningLatched: false,
			retired: false,
			revision: 0,
			selectionCursor: 0,
			nextReplicaIndex: 0,
			reconcileScheduledAt: null,
			admissions: {},
		}),
		onWake: async (c: AnyActorContext) => {
			const state = c.state as ScalerState;
			const strandedKeys = [...(state.warmingReplicaKeys ?? [])];
			const strandedCount = Math.max(
				state.warmingReplicas ?? 0,
				strandedKeys.length,
			);
			state.warmingReplicaKeys = [];
			state.warmingReplicas = 0;
			if (strandedCount > 0) {
				c.log.warn({
					msg: "recovering stranded AgentOS Apps replica warm reservations",
					strandedReservations: strandedCount,
				});
				const cleanup = await Promise.allSettled(
					strandedKeys.map((key) =>
						(
							c.client()[REPLICA_ACTOR_NAME].getOrCreate(key) as ReplicaHandle
						).destroy(),
					),
				);
				for (const result of cleanup) {
					if (result.status === "rejected") {
						c.log.error({
							msg: "failed to destroy a stranded AgentOS Apps warming replica",
							error: result.reason,
						});
					}
				}
				state.revision += 1;
			}
			if (
				state.appId &&
				state.release &&
				state.region &&
				state.scaling &&
				!state.retired &&
				state.replicas.length < state.scaling.minReplicas
			) {
				state.reconcileScheduledAt = Date.now() + 1;
				await c.schedule.after(1, "reconcile");
			}
			if (
				state.retired &&
				state.replicas.length === 0 &&
				state.warmingReplicas === 0
			) {
				c.destroy();
			}
		},
		actions: {
			prepare: async (
				c: AnyActorContext,
				input: {
					appId: string;
					release: StoredAppRelease;
					region: string;
					verifyReplica?: boolean;
				},
			) =>
				c.keepAwake(
					(async () => {
						await serialized(`scaler:${c.actorId}`, async () => {
							const state = c.state as ScalerState;
							if (state.release && state.release !== input.release.release) {
								fail(
									"agentos_apps_scaler_key_collision",
									"regional scaler key was reused for a different release",
								);
							}
							state.appId = input.appId;
							state.release = input.release.release;
							state.region = input.region;
							state.scaling = input.release.scaling;
							state.retired = false;
							state.selectionCursor ??= 0;
							state.nextReplicaIndex ??=
								Math.max(
									-1,
									...state.replicas.map((replica) =>
										Number(replica.key.at(-1) ?? -1),
									),
								) + 1;
							state.reconcileScheduledAt ??= null;
							state.admissions ??= {};
							state.warmingReplicaKeys ??= [];
							state.warmingReplicas = state.warmingReplicaKeys.length;
							state.capacityWarningLatched ??= false;
							for (const replica of state.replicas) {
								replica.activeRequests ??= 0;
								replica.lastUsedAt ??= replica.readyAt;
								replica.draining ??= false;
							}
						});
						const target = Math.max(
							input.release.scaling.minReplicas,
							input.verifyReplica ? 1 : 0,
						);
						while (
							(c.state as ScalerState).replicas.length +
								(c.state as ScalerState).warmingReplicas <
							target
						) {
							if (!(await addReplica(c))) break;
						}
						const state = requireScalerState(c);
						if (
							state.replicas.length > state.scaling.minReplicas &&
							state.reconcileScheduledAt === null
						) {
							state.reconcileScheduledAt = Date.now() + warmIdleTimeoutMs;
							await c.schedule.after(warmIdleTimeoutMs, "reconcile");
						}
						return {
							release: state.release,
							region: state.region,
							readyReplicas: state.replicas.length,
						};
					})(),
				),
			acquire: async (c: AnyActorContext) =>
				c.keepAwake(
					(async () => {
						const startedAt = Date.now();
						await serialized(`scaler:${c.actorId}`, async () => {
							const state = requireScalerState(c);
							expireAdmissions(c, state);
							if (Object.keys(state.admissions ?? {}).length >= maxAdmissions) {
								fail(
									"agentos_apps_admission_limit",
									`regional scaler reached maxAdmissions ${maxAdmissions}; raise maxAdmissions or investigate abandoned requests`,
									{ maxAdmissions },
								);
							}
							await reconcileIdleReplicas(c, state);
						});
						let coldStart = false;
						if ((c.state as ScalerState).replicas.length === 0) {
							coldStart = await addReplica(c);
							if (!coldStart) {
								const deadline = Date.now() + warmTimeoutMs;
								while (
									(c.state as ScalerState).replicas.length === 0 &&
									Date.now() < deadline
								) {
									await new Promise((resolve) => setTimeout(resolve, 25));
								}
							}
						}
						const shouldWarm = await serialized(
							`scaler:${c.actorId}`,
							async () => {
								const state = requireScalerState(c);
								const candidates = state.replicas.filter(
									(replica) => !replica.draining,
								);
								const minimum =
									candidates.length === 0
										? Number.POSITIVE_INFINITY
										: Math.min(
												...candidates.map((replica) => replica.activeRequests),
											);
								return (
									minimum + 1 >= state.scaling.targetConcurrency &&
									state.replicas.length + state.warmingReplicas <
										state.scaling.maxReplicas
								);
							},
						);
						if (shouldWarm) {
							void c.keepAwake(addReplica(c)).catch((error) => {
								c.log.error({
									msg: "AgentOS Apps background replica warm failed",
									error,
								});
							});
						}
						return serialized(`scaler:${c.actorId}`, async () => {
							const state = requireScalerState(c);
							let candidates = state.replicas.filter(
								(replica) => !replica.draining,
							);
							if (candidates.length === 0) {
								const reusable = state.replicas.find(
									(replica) => replica.draining && replica.activeRequests === 0,
								);
								if (reusable) {
									reusable.draining = false;
									candidates = [reusable];
								}
							}
							if (candidates.length === 0) {
								fail(
									"agentos_apps_no_capacity",
									"regional scaler has no ready replicas",
								);
							}
							const minimum = Math.min(
								...candidates.map((replica) => replica.activeRequests),
							);
							const leastLoaded = candidates.filter(
								(replica) => replica.activeRequests === minimum,
							);
							const selected =
								leastLoaded[state.selectionCursor++ % leastLoaded.length];
							if (!selected)
								fail(
									"agentos_apps_no_capacity",
									"regional scaler has no ready replicas",
								);
							selected.activeRequests += 1;
							selected.lastUsedAt = Date.now();
							const admissionId = randomUUID();
							state.admissions ??= {};
							state.admissions[admissionId] = {
								id: admissionId,
								replicaKey: selected.key,
								expiresAt: Date.now() + admissionLeaseMs,
							};
							if (state.reconcileScheduledAt === null) {
								state.reconcileScheduledAt = Date.now() + admissionLeaseMs;
								await c.schedule.after(admissionLeaseMs, "reconcile");
							}
							state.revision += 1;
							return {
								admissionId,
								leaseMs: admissionLeaseMs,
								key: selected.key,
								release: state.release,
								region: state.region,
								replicaCount: state.replicas.length,
								queueDelayMs: Date.now() - startedAt,
								coldStart,
							} satisfies ReplicaAdmission;
						});
					})(),
				),
			renew: async (c: AnyActorContext, admissionId: string) =>
				c.keepAwake(
					serialized(`scaler:${c.actorId}`, async () => {
						const state = requireScalerState(c);
						expireAdmissions(c, state);
						const admission = state.admissions?.[admissionId];
						if (!admission) return { renewed: false };
						admission.expiresAt = Date.now() + admissionLeaseMs;
						return { renewed: true, expiresAt: admission.expiresAt };
					}),
				),
			release: async (c: AnyActorContext, admissionId: string) =>
				c.keepAwake(
					serialized(`scaler:${c.actorId}`, async () => {
						const state = requireScalerState(c);
						expireAdmissions(c, state);
						const admission = state.admissions?.[admissionId];
						if (!admission) return { released: false };
						delete state.admissions?.[admissionId];
						const replica = state.replicas.find(
							(candidate) =>
								candidate.key.join("\0") === admission.replicaKey.join("\0"),
						);
						if (!replica) return { released: false };
						replica.activeRequests = Math.max(0, replica.activeRequests - 1);
						replica.lastUsedAt = Date.now();
						state.revision += 1;
						if (replica.draining) await reconcileIdleReplicas(c, state);
						if (
							state.replicas.length > state.scaling.minReplicas &&
							state.reconcileScheduledAt === null
						) {
							state.reconcileScheduledAt = Date.now() + warmIdleTimeoutMs;
							await c.schedule.after(warmIdleTimeoutMs, "reconcile");
						}
						if (
							state.retired &&
							state.replicas.length === 0 &&
							state.warmingReplicas === 0
						) {
							c.destroy();
						}
						return { released: true };
					}),
				),
			reconcile: async (c: AnyActorContext) =>
				c.keepAwake(
					serialized(`scaler:${c.actorId}`, async () => {
						const state = requireScalerState(c);
						state.reconcileScheduledAt = null;
						const removed = await reconcileIdleReplicas(c, state);
						const missing = Math.max(
							0,
							state.scaling.minReplicas -
								state.replicas.length -
								state.warmingReplicas,
						);
						const deadlines = Object.values(state.admissions ?? {}).map(
							(admission) => admission.expiresAt,
						);
						if (state.replicas.length > state.scaling.minReplicas) {
							deadlines.push(
								...state.replicas.map((replica) =>
									replica.activeRequests === 0
										? replica.lastUsedAt + warmIdleTimeoutMs
										: Date.now() + warmIdleTimeoutMs,
								),
							);
						}
						if (deadlines.length > 0) {
							const nextDelay = Math.max(
								1,
								Math.min(...deadlines) - Date.now(),
							);
							state.reconcileScheduledAt = Date.now() + nextDelay;
							await c.schedule.after(nextDelay, "reconcile");
						}
						for (let index = 0; index < missing; index += 1) {
							void c.keepAwake(addReplica(c)).catch((error) => {
								c.log.error({
									msg: "AgentOS Apps minimum replica warm failed",
									error,
								});
							});
						}
						if (
							state.retired &&
							state.replicas.length === 0 &&
							state.warmingReplicas === 0
						) {
							c.destroy();
						}
						return { removed, readyReplicas: state.replicas.length };
					}),
				),
			drainReplica: async (c: AnyActorContext, key: string[]) =>
				c.keepAwake(
					serialized(`scaler:${c.actorId}`, async () => {
						const state = requireScalerState(c);
						const replica = state.replicas.find(
							(candidate) => candidate.key.join("\0") === key.join("\0"),
						);
						if (!replica) {
							fail(
								"agentos_apps_replica_not_found",
								"replica is not in this scaler",
							);
						}
						if (state.replicas.length <= state.scaling.minReplicas) {
							void c.keepAwake(addReplica(c)).catch((error) => {
								c.log.error({
									msg: "AgentOS Apps replacement replica warm failed",
									error,
								});
							});
						}
						replica.draining = true;
						state.revision += 1;
						await reconcileIdleReplicas(c, state);
						return { draining: replica.activeRequests > 0 };
					}),
				),
			retire: async (c: AnyActorContext) =>
				c.keepAwake(
					serialized(`scaler:${c.actorId}`, async () => {
						const state = requireScalerState(c);
						state.scaling = { ...state.scaling, minReplicas: 0 };
						state.retired = true;
						for (const replica of state.replicas) replica.draining = true;
						state.revision += 1;
						const removed = await reconcileIdleReplicas(c, state);
						if (state.replicas.length === 0 && state.warmingReplicas === 0)
							c.destroy();
						return {
							removed,
							drainingReplicas: state.replicas.length,
						};
					}),
				),
			inspect: (c: AnyActorContext) => {
				const state = c.state as ScalerState;
				return {
					region: state.region,
					release: state.release,
					scaling: state.scaling,
					revision: state.revision,
					readyReplicas: state.replicas,
					warmingReplicas: state.warmingReplicas,
				};
			},
		},
	});

	const buildVmOptions: AgentOsOptions = {
		defaultSoftware: false,
		software: [sh, tar, appsBuilder],
		permissions: {
			fs: "allow",
			childProcess: "allow",
			process: "allow",
			env: "allow",
			network: "allow",
		},
		limits: {
			tls: {
				maxBufferedBytes: 16 * 1024 * 1024,
			},
			jsRuntime: {
				v8HeapLimitMb: 1_024,
			},
			resources: {
				maxProcesses: 64,
				maxOpenFds: 2_048,
				// Leave framing headroom beneath the sidecar's 16 MiB bridge cap.
				maxPreadBytes: 15 * 1024 * 1024,
				maxFdWriteBytes: 16 * 1024 * 1024,
				maxSocketBufferedBytes: 16 * 1024 * 1024,
				// Packaging temporarily stores both the installed application tree and
				// its uncompressed tar. Keep that workspace bounded, but large enough
				// for dependency-heavy packages such as the published RivetKit build.
				maxFilesystemBytes: Math.max(
					DEFAULT_MAX_BUILD_FILESYSTEM_BYTES,
					maxBuildArtifactBytes * 2,
				),
			},
		},
	};
	const createBuildVm = async (): Promise<BuildHandle> => {
		// The artifact stays on the guest filesystem and is read back over the
		// host VM API instead of a writable host_dir mount: guest-side access
		// to host_dir mounts fails with "Permission denied (os error 2)"
		// (rivet-dev/agentos#1872), which made every pack step fail. Reading
		// through vm.readFile also removes the host temp directory and its
		// cleanup entirely.
		const artifactGuestPath = "/agentos-app.tar";
		const vm = await AgentOs.create(buildVmOptions);
		let artifact: Uint8Array | undefined;
		const readArtifact = async (): Promise<Uint8Array> => {
			artifact ??= new Uint8Array(await vm.readFile(artifactGuestPath));
			return artifact;
		};
		return {
			artifactGuestPath,
			writeFiles: (...args) => vm.writeFiles(...args),
			execArgv: (...args) => vm.execArgv(...args),
			artifactSize: async () => (await readArtifact()).byteLength,
			readArtifact,
			dispose: async () => {
				await vm.dispose();
			},
		};
	};

	const temporaryArtifacts = new Map<
		string,
		{ directory: string; path: string }
	>();
	const guestEngineRegistrations = new Map<
		string,
		GuestEngineProxyRegistration
	>();
	type GuestRpcHead = {
		status: number;
		statusText: string;
		headers: Array<[string, string]>;
	};
	type PendingGuestRpc = {
		resolveHead(response: GuestRpcHead): void;
		reject(error: unknown): void;
		timeout: ReturnType<typeof setTimeout>;
		controller?: ReadableStreamDefaultController<Uint8Array>;
		chunk?: Uint8Array;
		ended: boolean;
		headResolved: boolean;
	};
	type GuestBridge = {
		vm: AgentOs;
		pid: number;
		stdoutBuffer: string;
		pending: Map<string, PendingGuestRpc>;
	};
	const guestBridges = new Map<string, GuestBridge>();

	const rejectGuestBridge = (actorId: string, error: unknown) => {
		const bridge = guestBridges.get(actorId);
		if (!bridge) return;
		guestBridges.delete(actorId);
		for (const pending of bridge.pending.values()) {
			clearTimeout(pending.timeout);
			if (pending.headResolved) pending.controller?.error(error);
			else pending.reject(error);
		}
		bridge.pending.clear();
	};

	const sendGuestRpcControl = (
		c: AnyActorContext,
		bridge: GuestBridge,
		id: string,
		event: "ack" | "cancel",
	) => {
		void bridge.vm
			.writeProcessStdin(bridge.pid, `${JSON.stringify({ id, event })}\n`)
			.catch((error) => {
				const pending = bridge.pending.get(id);
				bridge.pending.delete(id);
				if (pending) {
					clearTimeout(pending.timeout);
					if (pending.headResolved) pending.controller?.error(error);
					else pending.reject(error);
				}
				c.log.error({
					msg: "failed to send AgentOS Apps guest RPC control",
					event,
					error,
				});
			});
	};

	const pumpGuestRpc = (
		c: AnyActorContext,
		bridge: GuestBridge,
		id: string,
		pending: PendingGuestRpc,
	) => {
		if (
			pending.controller &&
			pending.chunk &&
			(pending.controller.desiredSize ?? 1) > 0
		) {
			const chunk = pending.chunk;
			pending.chunk = undefined;
			pending.controller.enqueue(chunk);
			sendGuestRpcControl(c, bridge, id, "ack");
		}
		if (pending.controller && pending.ended && !pending.chunk) {
			bridge.pending.delete(id);
			pending.controller.close();
		}
	};

	const handleGuestStdout = (c: any, bridge: GuestBridge, data: Uint8Array) => {
		bridge.stdoutBuffer += new TextDecoder().decode(data);
		const maxBufferedCharacters =
			Math.ceil((maxResponseBytes * 4) / 3) + 65_536;
		if (bridge.stdoutBuffer.length > maxBufferedCharacters) {
			const error = new AgentOSAppsError(
				"agentos_apps_guest_rpc_output_limit",
				`guest RPC output exceeded ${maxBufferedCharacters} buffered characters`,
				{ limit: maxBufferedCharacters },
			);
			rejectGuestBridge(c.actorId, error);
			c.log.error({ msg: error.message, error });
			return;
		}
		for (;;) {
			const newline = bridge.stdoutBuffer.indexOf("\n");
			if (newline < 0) break;
			const line = bridge.stdoutBuffer.slice(0, newline);
			bridge.stdoutBuffer = bridge.stdoutBuffer.slice(newline + 1);
			if (line.startsWith(GUEST_RPC_PREFIX)) {
				let response: {
					id?: unknown;
					event?: unknown;
					status?: unknown;
					statusText?: unknown;
					headers?: unknown;
					bodyBase64?: unknown;
					message?: unknown;
				};
				try {
					response = JSON.parse(line.slice(GUEST_RPC_PREFIX.length));
				} catch (error) {
					c.log.error({
						msg: "AgentOS Apps guest emitted an invalid RPC response",
						error,
					});
					continue;
				}
				if (typeof response.id !== "string") {
					c.log.error({
						msg: "AgentOS Apps guest RPC response omitted its request id",
					});
					continue;
				}
				const pending = bridge.pending.get(response.id);
				if (!pending) {
					c.log.warn({
						msg: "AgentOS Apps guest emitted an RPC response for an unknown request",
						requestId: response.id,
					});
					continue;
				}
				if (
					response.event === "head" &&
					typeof response.status === "number" &&
					typeof response.statusText === "string" &&
					Array.isArray(response.headers)
				) {
					clearTimeout(pending.timeout);
					pending.headResolved = true;
					pending.resolveHead({
						status: response.status,
						statusText: response.statusText,
						headers: response.headers as Array<[string, string]>,
					});
					continue;
				}
				if (
					response.event === "chunk" &&
					typeof response.bodyBase64 === "string"
				) {
					const chunk = Buffer.from(response.bodyBase64, "base64");
					if (chunk.byteLength > maxResponseBytes || pending.chunk) {
						const error = new AgentOSAppsError(
							"agentos_apps_guest_rpc_output_limit",
							"guest RPC exceeded its bounded streaming buffer",
							{ limit: maxResponseBytes },
						);
						bridge.pending.delete(response.id);
						sendGuestRpcControl(c, bridge, response.id, "cancel");
						pending.controller?.error(error);
						continue;
					}
					pending.chunk = chunk;
					pumpGuestRpc(c, bridge, response.id, pending);
					continue;
				}
				if (response.event === "end") {
					pending.ended = true;
					pumpGuestRpc(c, bridge, response.id, pending);
					continue;
				}
				if (response.event === "error") {
					const error = new AgentOSAppsError(
						"agentos_apps_guest_rpc_failed",
						typeof response.message === "string"
							? response.message
							: "guest RPC failed",
					);
					bridge.pending.delete(response.id);
					clearTimeout(pending.timeout);
					if (pending.headResolved) pending.controller?.error(error);
					else pending.reject(error);
					continue;
				}
				c.log.error({
					msg: "AgentOS Apps guest emitted an invalid RPC event",
					requestId: response.id,
					event: response.event,
				});
				continue;
			}
			if (line) {
				const output = boundedOutput(line, DEFAULT_MAX_BUILD_OUTPUT_BYTES);
				c.log.info({
					msg: "AgentOS Apps guest stdout",
					pid: bridge.pid,
					output,
				});
				c.broadcast("processOutput", {
					pid: bridge.pid,
					stream: "stdout",
					data: new TextEncoder().encode(`${line}\n`),
				});
			}
		}
	};

	const cleanupTemporaryArtifact = async (
		c: AnyActorContext,
	): Promise<void> => {
		const temporary = temporaryArtifacts.get(c.actorId);
		if (!temporary) return;
		await rm(temporary.directory, { recursive: true, force: true });
		temporaryArtifacts.delete(c.actorId);
	};

	const agentOSAppsReplica = agentOS<ReplicaState>({
		onVmStart: async (c: any, vm: AgentOs) => {
			const state = c.state as ReplicaState;
			const configuration = state.configuration;
			if (!configuration) {
				fail(
					"agentos_apps_replica_unconfigured",
					"execution replica must be configured before its VM starts",
				);
			}
			const runtime = configuration.runtime;
			if (configuration.usesRivetKit) {
				const connection = resolveDefaultRivetConnection();
				if (
					connection.endpoint.replace(/\/$/, "") !==
					runtime.endpoint.replace(/\/$/, "")
				) {
					fail(
						"agentos_apps_runtime_changed",
						"the host Rivet endpoint changed for an immutable execution replica",
					);
				}
			}
			const pendingStdout: Uint8Array[] = [];
			let bridge: GuestBridge | undefined;
			const engineRegistration = guestEngineRegistrations.get(c.actorId);
			if (configuration.usesRivetKit && !engineRegistration) {
				fail(
					"agentos_apps_guest_engine_proxy_missing",
					"RivetKit execution replica is missing its scoped Engine capability",
				);
			}
			const process = vm.spawn("node", ["/app/main.mjs"], {
				cwd: "/app",
				// Never forward the host's RIVET_TOKEN. RivetKit releases receive
				// only non-secret placement metadata.
				env: replicaGuestEnvironment(
					configuration,
					engineRegistration?.endpoint,
				),
				onStdout: (data) => {
					if (bridge) handleGuestStdout(c, bridge, data);
					else pendingStdout.push(data);
				},
				onStderr: (data) => {
					const output = boundedOutput(
						new TextDecoder().decode(data),
						DEFAULT_MAX_BUILD_OUTPUT_BYTES,
					);
					c.log.error({
						msg: "AgentOS Apps guest stderr",
						pid: process.pid,
						output,
					});
					c.broadcast("processOutput", {
						pid: process.pid,
						stream: "stderr",
						data,
					});
				},
			});
			bridge = {
				vm,
				pid: process.pid,
				stdoutBuffer: "",
				pending: new Map(),
			};
			guestBridges.set(c.actorId, bridge);
			for (const data of pendingStdout) handleGuestStdout(c, bridge, data);
			state.guestPid = process.pid;
			void c
				.keepAwake(
					vm.waitProcess(process.pid).then((exitCode) => {
						c.broadcast("processExit", { pid: process.pid, exitCode });
						if (exitCode === 0) {
							c.log.info({
								msg: "AgentOS Apps guest process exited",
								pid: process.pid,
								exitCode,
							});
						} else {
							c.log.error({
								msg: "AgentOS Apps guest process exited",
								pid: process.pid,
								exitCode,
							});
						}
						return exitCode;
					}),
				)
				.catch((error: unknown) =>
					c.log.error({
						msg: "AgentOS Apps guest process wait failed",
						pid: process.pid,
						error,
					}),
				);
		},
		onVmStop: async (
			c: any,
			vm: AgentOs,
			reason: "sleep" | "destroy" | "error",
		) => {
			const state = c.state as ReplicaState;
			const guestPid = state.guestPid;
			rejectGuestBridge(
				c.actorId,
				new AgentOSAppsError(
					"agentos_apps_guest_stopped",
					`AgentOS Apps guest stopped while requests were pending (${reason})`,
				),
			);
			if (typeof guestPid === "number") {
				vm.stopProcess(guestPid);
				let timeout: ReturnType<typeof setTimeout> | undefined;
				const exited = await Promise.race([
					vm.waitProcess(guestPid).then(() => true),
					new Promise<false>((resolve) => {
						timeout = setTimeout(
							() => resolve(false),
							GUEST_SHUTDOWN_TIMEOUT_MS,
						);
					}),
				]);
				if (timeout) clearTimeout(timeout);
				if (!exited) {
					c.log.warn({
						msg: "AgentOS Apps guest shutdown timed out; disposing VM",
						guestPid,
						timeoutMs: GUEST_SHUTDOWN_TIMEOUT_MS,
						reason,
					});
				}
				state.guestPid = null;
			}
		},
		onVmDisposed: async (c: any) => {
			rejectGuestBridge(
				c.actorId,
				new AgentOSAppsError(
					"agentos_apps_guest_disposed",
					"AgentOS Apps guest VM was disposed",
				),
			);
			await cleanupTemporaryArtifact(c);
			unregisterGuestEngineProxy(c.actorId);
			guestEngineRegistrations.delete(c.actorId);
		},
		options: {
			noSleep: true,
		},
		inspector: {
			tabs: [
				{
					id: "agentos-replica",
					label: "Execution replica",
					icon: "cpu",
					source: `${INSPECTOR_ROOT}/replica`,
				},
			],
		},
		mounts: undefined,
		state: {
			configuration: null,
			startedAt: null,
			guestPid: null,
		} as ReplicaState,
		permissions: {
			fs: "allow",
			childProcess: "allow",
			process: "allow",
			env: "allow",
			network: "allow",
		},
		limits: {
			http: {
				maxFetchResponseBytes: maxResponseBytes,
			},
		},
		resolveOptions: async (c: any) => {
			const state = c.state as ReplicaState;
			if (!state.configuration) {
				fail(
					"agentos_apps_replica_unconfigured",
					"execution replica must be configured with an artifact before its VM boots",
				);
			}
			await cleanupTemporaryArtifact(c);
			unregisterGuestEngineProxy(c.actorId);
			guestEngineRegistrations.delete(c.actorId);
			const directory = await mkdtemp(join(tmpdir(), "agentos-apps-replica-"));
			const path = join(directory, `${state.configuration.release}.aospkg`);
			try {
				const appHandle = c
					.client()
					[APP_ACTOR_NAME].getOrCreate([state.configuration.appId]);
				const manifest = (await appHandle.getArtifactManifest(
					state.configuration.release,
				)) as {
					hash: string;
					bytes: number;
					chunks: number;
					chunkBytes: number;
				};
				if (
					manifest.hash !== state.configuration.artifactHash ||
					manifest.bytes !== state.configuration.artifactBytes ||
					manifest.chunks > MAX_ARTIFACT_CHUNKS
				) {
					fail(
						"agentos_apps_artifact_manifest_mismatch",
						"replica artifact manifest does not match its immutable configuration",
					);
				}
				const digest = createHash("sha256");
				const artifactFile = await open(path, "wx", 0o600);
				let bytes = 0;
				try {
					for (let index = 0; index < manifest.chunks; index += 1) {
						const chunk = new Uint8Array(
							await appHandle.readArtifactChunk(
								state.configuration.release,
								index,
							),
						);
						bytes += chunk.byteLength;
						if (
							chunk.byteLength > ARTIFACT_CHUNK_BYTES ||
							bytes > manifest.bytes
						) {
							fail(
								"agentos_apps_artifact_chunk_invalid",
								"replica received an invalid artifact chunk length",
							);
						}
						digest.update(chunk);
						await artifactFile.writeFile(chunk);
					}
				} finally {
					await artifactFile.close();
				}
				const hash = digest.digest("hex");
				if (bytes !== manifest.bytes || hash !== manifest.hash) {
					fail(
						"agentos_apps_artifact_hash_mismatch",
						"rehydrated replica artifact failed size or hash verification",
						{
							expectedBytes: manifest.bytes,
							actualBytes: bytes,
							expectedHash: manifest.hash,
							actualHash: hash,
						},
					);
				}
				temporaryArtifacts.set(c.actorId, { directory, path });
			} catch (error) {
				await rm(directory, { recursive: true, force: true }).catch(
					(removeError) => {
						c.log.error({
							msg: "failed to remove unsuccessful replica artifact",
							removeError,
						});
					},
				);
				throw error;
			}
			let engineRegistration: GuestEngineProxyRegistration | undefined;
			if (state.configuration.usesRivetKit) {
				const connection = resolveDefaultRivetConnection();
				if (
					connection.endpoint.replace(/\/$/, "") !==
					state.configuration.runtime.endpoint.replace(/\/$/, "")
				) {
					fail(
						"agentos_apps_runtime_changed",
						"the host Rivet endpoint changed for an immutable execution replica",
					);
				}
				engineRegistration = await registerGuestEngineProxy({
					owner: c.actorId,
					upstreamEndpoint: state.configuration.runtime.endpoint,
					upstreamToken: connection.token,
					namespace: state.configuration.runtime.namespace,
					pool: state.configuration.runtime.pool,
					maxRequestBytes,
					maxResponseBytes,
				});
				guestEngineRegistrations.set(c.actorId, engineRegistration);
			}
			return {
				loopbackExemptPorts: replicaLoopbackExemptPorts(
					state.configuration,
					engineRegistration?.port,
				),
				mounts: [
					{
						path: "/app",
						plugin: {
							id: "agentos_packages",
							config: {
								kind: "tar",
								tarPath: path,
								root: "/",
								readOnly: true,
							},
						},
						readOnly: true,
					},
				],
			};
		},
		onRequest: async (c: any, request: Request): Promise<Response> => {
			const pathname = new URL(request.url).pathname;
			if (
				pathname !== "/api/rivet/metadata" &&
				pathname !== "/api/rivet/start"
			) {
				return new Response("Not Found", { status: 404 });
			}
			const bridge = guestBridges.get(c.actorId);
			if (!bridge) {
				return new Response("AgentOS Apps guest is not ready", { status: 503 });
			}
			if (bridge.pending.size >= MAX_PENDING_GUEST_RPCS) {
				return new Response("AgentOS Apps guest request limit exceeded", {
					status: 503,
				});
			}
			const body = await readBoundedRequestBody(request, maxRequestBytes);
			if (body === null) {
				return new Response("Request body exceeds AgentOS Apps limit", {
					status: 413,
				});
			}
			const headers: Record<string, string> = {};
			request.headers.forEach((value, name) => {
				headers[name] = value;
			});
			const configuration = (c.state as ReplicaState).configuration;
			const engineRegistration = guestEngineRegistrations.get(c.actorId);
			if (configuration?.usesRivetKit) {
				if (!engineRegistration) {
					return new Response("AgentOS Apps Engine capability is not ready", {
						status: 503,
					});
				}
				headers["x-rivet-endpoint"] = engineRegistration.endpoint;
				headers["x-rivet-namespace-name"] = configuration.runtime.namespace;
				headers["x-rivet-pool-name"] = configuration.runtime.pool;
				delete headers["x-rivet-token"];
				delete headers.authorization;
			}
			const id = randomUUID();
			let pending!: PendingGuestRpc;
			const headPromise = new Promise<GuestRpcHead>((resolve, reject) => {
				pending = {
					resolveHead: resolve,
					reject,
					timeout: setTimeout(() => {
						bridge.pending.delete(id);
						sendGuestRpcControl(c, bridge, id, "cancel");
						reject(
							new AgentOSAppsError(
								"agentos_apps_guest_rpc_timeout",
								`guest RivetKit response headers exceeded ${GUEST_RPC_TIMEOUT_MS} ms`,
								{ timeoutMs: GUEST_RPC_TIMEOUT_MS },
							),
						);
					}, GUEST_RPC_TIMEOUT_MS),
					ended: false,
					headResolved: false,
				};
				bridge.pending.set(id, pending);
			});
			try {
				await bridge.vm.writeProcessStdin(
					bridge.pid,
					`${JSON.stringify({
						id,
						method: request.method,
						url: request.url,
						headers,
						bodyBase64:
							body && body.byteLength > 0
								? Buffer.from(body).toString("base64")
								: undefined,
					})}\n`,
				);
			} catch (error) {
				bridge.pending.delete(id);
				clearTimeout(pending.timeout);
				pending.reject(error);
			}
			const head = await headPromise;
			const stream = new ReadableStream<Uint8Array>(
				{
					start(controller) {
						pending.controller = controller;
						pumpGuestRpc(c, bridge, id, pending);
					},
					pull() {
						pumpGuestRpc(c, bridge, id, pending);
					},
					cancel() {
						bridge.pending.delete(id);
						sendGuestRpcControl(c, bridge, id, "cancel");
					},
				},
				{ highWaterMark: 1 },
			);
			return new Response(stream, {
				status: head.status,
				statusText: head.statusText,
				headers: head.headers,
			});
		},
		actions: {
			destroy: (c: any) => c.destroy(),
			configure: (c: any, input: ReplicaState["configuration"]) => {
				if (!input) {
					fail(
						"agentos_apps_replica_invalid_config",
						"execution replica configuration is required",
					);
				}
				const state = c.state as ReplicaState;
				if (
					state.configuration &&
					(state.configuration.release !== input.release ||
						state.configuration.artifactHash !== input.artifactHash ||
						Boolean(state.configuration.usesRivetKit) !==
							Boolean(input.usesRivetKit))
				) {
					fail(
						"agentos_apps_replica_config_collision",
						"execution replica cannot be reassigned to another immutable release",
					);
				}
				state.configuration = {
					...input,
					usesRivetKit: Boolean(input.usesRivetKit),
				};
			},
			markStarted: (c: any) => {
				(c.state as ReplicaState).startedAt = Date.now();
			},
			inspect: (c: any) => {
				const state = c.state as ReplicaState;
				return {
					release: state.configuration?.release ?? null,
					artifactHash: state.configuration?.artifactHash ?? null,
					namespace: state.configuration?.namespace ?? null,
					pool: state.configuration?.runtime.pool ?? null,
					startedAt: state.startedAt,
				};
			},
		},
	});

	return {
		agentOSAppsApp,
		agentOSAppsScaler,
		agentOSAppsReplica,
	};
}
