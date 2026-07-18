import { z } from "zod/v4";
import type {
	AgentExitHandler,
	AgentOsOptions,
	AgentStderrHandler,
	LimitWarningHandler,
	NativeMountConfig,
} from "./agent-os.js";
import type { Binding, Bindings } from "./bindings.js";

const stringArray = z.array(z.string());
const nonNegativeInteger = z.number().int().nonnegative();
const positiveInteger = z.number().int().positive();
const vmIdSchema = z.number().int().min(0).max(0xffffffff);
const maxAccountRecordBytes = 4095;
const maxGroupMembers = 256;
const utf8Encoder = new TextEncoder();
const utf8ByteLength = (value: string) => utf8Encoder.encode(value).byteLength;
const vmAccountNameSchema = z
	.string()
	.min(1)
	.refine((value) => !/[:,\s\0]/u.test(value), "Invalid account name")
	.refine(
		(value) => utf8ByteLength(value) <= maxAccountRecordBytes,
		`Account text exceeds ${maxAccountRecordBytes} UTF-8 bytes`,
	);
const vmGuestPathSchema = z
	.string()
	.startsWith("/")
	.refine((value) => !/[:\r\n\0]/u.test(value), "Invalid account path")
	.refine(
		(value) => utf8ByteLength(value) <= maxAccountRecordBytes,
		`Account text exceeds ${maxAccountRecordBytes} UTF-8 bytes`,
	);
const vmGecosSchema = z
	.string()
	.refine((value) => !/[:\r\n\0]/u.test(value), "Invalid GECOS field")
	.refine(
		(value) => utf8ByteLength(value) <= maxAccountRecordBytes,
		`Account text exceeds ${maxAccountRecordBytes} UTF-8 bytes`,
	);
const passwdRecordBytes = (account: {
	uid: number;
	gid: number;
	username: string;
	homedir: string;
	shell: string;
	gecos?: string;
}) =>
	7 +
	utf8ByteLength(account.username) +
	String(account.uid).length +
	String(account.gid).length +
	utf8ByteLength(account.gecos ?? "") +
	utf8ByteLength(account.homedir) +
	utf8ByteLength(account.shell);
const groupRecordBytes = (group: {
	gid: number;
	name: string;
	members: string[];
}) =>
	4 +
	utf8ByteLength(group.name) +
	String(group.gid).length +
	group.members.reduce((total, member) => total + utf8ByteLength(member), 0) +
	Math.max(0, group.members.length - 1);
const vmUserAccountSchema = z
	.object({
		uid: vmIdSchema,
		gid: vmIdSchema,
		username: vmAccountNameSchema,
		homedir: vmGuestPathSchema,
		shell: vmGuestPathSchema,
		gecos: vmGecosSchema.optional(),
		supplementaryGids: z.array(vmIdSchema).max(64),
	})
	.strict()
	.superRefine((account, context) => {
		if (passwdRecordBytes(account) > maxAccountRecordBytes) {
			context.addIssue({
				code: "custom",
				message: `Rendered passwd record exceeds ${maxAccountRecordBytes} bytes (the 4096-byte ABI buffer includes its terminating NUL)`,
			});
		}
	});
const vmGroupSchema = z
	.object({
		gid: vmIdSchema,
		name: vmAccountNameSchema,
		members: z.array(vmAccountNameSchema).max(maxGroupMembers),
	})
	.strict()
	.superRefine((group, context) => {
		if (groupRecordBytes(group) > maxAccountRecordBytes) {
			context.addIssue({
				code: "custom",
				message: `Rendered group record exceeds ${maxAccountRecordBytes} bytes (the 4096-byte ABI buffer includes its terminating NUL)`,
			});
		}
	});
const vmUserConfigSchema = z
	.object({
		uid: vmIdSchema.optional(),
		gid: vmIdSchema.optional(),
		euid: vmIdSchema.optional(),
		egid: vmIdSchema.optional(),
		username: vmAccountNameSchema.optional(),
		homedir: vmGuestPathSchema.optional(),
		shell: vmGuestPathSchema.optional(),
		gecos: vmGecosSchema.optional(),
		groupName: vmAccountNameSchema.optional(),
		supplementaryGids: z.array(vmIdSchema).max(64).optional(),
		accounts: z.array(vmUserAccountSchema).max(64).optional(),
		groups: z.array(vmGroupSchema).max(128).optional(),
	})
	.strict()
	.superRefine((user, context) => {
		const primary = {
			uid: user.uid ?? 1000,
			gid: user.gid ?? 1000,
			username: user.username ?? "agentos",
			homedir: user.homedir ?? "/home/agentos",
			shell: user.shell ?? "/bin/sh",
			gecos: user.gecos,
			supplementaryGids: user.supplementaryGids ?? [],
		};
		if (passwdRecordBytes(primary) > maxAccountRecordBytes) {
			context.addIssue({
				code: "custom",
				message: `Rendered passwd record exceeds ${maxAccountRecordBytes} bytes (the 4096-byte ABI buffer includes its terminating NUL)`,
			});
		}

		const materialized = new Map<number, { name: string; members: string[] }>();
		const explicitNames = new Map<string, number>();
		for (const [index, group] of (user.groups ?? []).entries()) {
			if (materialized.has(group.gid)) {
				context.addIssue({
					code: "custom",
					path: ["groups", index, "gid"],
					message: `Duplicate user group gid ${group.gid}`,
				});
				continue;
			}
			const previousGid = explicitNames.get(group.name);
			if (previousGid !== undefined) {
				context.addIssue({
					code: "custom",
					path: ["groups", index, "name"],
					message: `Duplicate user group name ${group.name}`,
				});
			}
			explicitNames.set(group.name, group.gid);
			materialized.set(group.gid, {
				name: group.name,
				members: [...group.members],
			});
		}

		if (!materialized.has(primary.gid)) {
			materialized.set(primary.gid, {
				name: user.groupName ?? primary.username,
				members: [primary.username],
			});
		}
		const authoritativeGids = new Set(materialized.keys());
		const effectiveAccounts = new Map(
			(user.accounts ?? []).map((account) => [account.uid, account] as const),
		);
		effectiveAccounts.set(primary.uid, primary);
		for (const account of effectiveAccounts.values()) {
			for (const gid of [account.gid, ...account.supplementaryGids]) {
				if (authoritativeGids.has(gid)) continue;
				const group = materialized.get(gid) ?? {
					name: `group${gid}`,
					members: [],
				};
				if (!group.members.includes(account.username)) {
					group.members.push(account.username);
				}
				materialized.set(gid, group);
			}
		}

		const gidsByName = new Map<string, number>();
		for (const [gid, group] of materialized) {
			const previousGid = gidsByName.get(group.name);
			if (previousGid !== undefined && previousGid !== gid) {
				context.addIssue({
					code: "custom",
					path: ["groups"],
					message: `Materialized user group name ${group.name} maps to both gid ${previousGid} and gid ${gid}; synthesized group names must not collide`,
				});
			}
			gidsByName.set(group.name, gid);
			if (
				group.members.length > maxGroupMembers ||
				groupRecordBytes({ gid, ...group }) > maxAccountRecordBytes
			) {
				context.addIssue({
					code: "custom",
					path: ["groups"],
					message: `Materialized group exceeds the ${maxGroupMembers}-member or ${maxAccountRecordBytes}-byte account ABI limit`,
				});
			}
		}
	});
const functionSchema = z.custom<(...args: any[]) => any>(
	(value) => typeof value === "function",
	{ message: "Expected function" },
);

const permissionModeSchema = z.enum(["allow", "deny"]);

const fsPermissionRuleSchema = z
	.object({
		mode: permissionModeSchema,
		operations: stringArray.optional(),
		paths: stringArray.optional(),
	})
	.strict();

const patternPermissionRuleSchema = z
	.object({
		mode: permissionModeSchema,
		operations: stringArray.optional(),
		patterns: stringArray.optional(),
	})
	.strict();

const fsRulePermissionsSchema = z
	.object({
		default: permissionModeSchema.optional(),
		rules: z.array(fsPermissionRuleSchema),
	})
	.strict();

const patternRulePermissionsSchema = z
	.object({
		default: permissionModeSchema.optional(),
		rules: z.array(patternPermissionRuleSchema),
	})
	.strict();

const fsPermissionsSchema = z.union([
	permissionModeSchema,
	fsRulePermissionsSchema,
]);
const patternPermissionsSchema = z.union([
	permissionModeSchema,
	patternRulePermissionsSchema,
]);

export const permissionsSchema = z
	.object({
		fs: fsPermissionsSchema.optional(),
		network: patternPermissionsSchema.optional(),
		childProcess: patternPermissionsSchema.optional(),
		process: patternPermissionsSchema.optional(),
		env: patternPermissionsSchema.optional(),
		binding: patternPermissionsSchema.optional(),
	})
	.strict();

export const agentOsLimitsSchema = z
	.object({
		resources: z
			.object({
				cpuCount: positiveInteger.optional(),
				maxProcesses: nonNegativeInteger.optional(),
				maxOpenFds: nonNegativeInteger.optional(),
				maxPipes: nonNegativeInteger.optional(),
				maxPtys: nonNegativeInteger.optional(),
				maxSockets: nonNegativeInteger.optional(),
				maxConnections: nonNegativeInteger.optional(),
				maxSocketBufferedBytes: nonNegativeInteger.optional(),
				maxSocketDatagramQueueLen: nonNegativeInteger.optional(),
				maxFilesystemBytes: nonNegativeInteger.optional(),
				maxInodeCount: nonNegativeInteger.optional(),
				maxBlockingReadMs: nonNegativeInteger.optional(),
				maxPreadBytes: nonNegativeInteger.optional(),
				maxFdWriteBytes: nonNegativeInteger.optional(),
				maxProcessArgvBytes: nonNegativeInteger.optional(),
				maxProcessEnvBytes: nonNegativeInteger.optional(),
				maxReaddirEntries: nonNegativeInteger.optional(),
				maxWasmMemoryBytes: nonNegativeInteger.optional(),
				maxWasmStackBytes: nonNegativeInteger.optional(),
			})
			.strict()
			.optional(),
		http: z
			.object({ maxFetchResponseBytes: positiveInteger.optional() })
			.strict()
			.optional(),
		tls: z
			.object({ maxBufferedBytes: positiveInteger.optional() })
			.strict()
			.optional(),
		bindings: z
			.object({
				defaultBindingTimeoutMs: nonNegativeInteger.optional(),
				maxBindingTimeoutMs: nonNegativeInteger.optional(),
				maxRegisteredCollections: positiveInteger.optional(),
				maxRegisteredBindingsPerVm: positiveInteger.optional(),
				maxBindingsPerCollection: positiveInteger.optional(),
				maxBindingSchemaBytes: positiveInteger.optional(),
				maxExamplesPerBinding: nonNegativeInteger.optional(),
				maxBindingExampleInputBytes: positiveInteger.optional(),
			})
			.strict()
			.optional(),
		plugins: z
			.object({
				maxPersistedManifestBytes: positiveInteger.optional(),
				maxPersistedManifestFileBytes: nonNegativeInteger.optional(),
			})
			.strict()
			.optional(),
		acp: z
			.object({
				maxReadLineBytes: positiveInteger.optional(),
				stdoutBufferByteLimit: positiveInteger.optional(),
				maxCompletedMessageBytes: positiveInteger.optional(),
				maxTurnOutputBytes: positiveInteger.optional(),
				maxPromptBytes: positiveInteger.optional(),
				maxPromptBlocks: positiveInteger.optional(),
				maxFallbackContinuationBytes: positiveInteger.optional(),
				maxSessionHistoryBytes: positiveInteger.optional(),
				maxSessionHistoryEvents: positiveInteger.optional(),
				maxHistoryPageEntries: positiveInteger.optional(),
				maxSessionListEntries: positiveInteger.optional(),
				maxSessionsPerVm: positiveInteger.optional(),
				maxPromptsPerSession: positiveInteger.optional(),
				maxPromptsPerVm: positiveInteger.optional(),
				maxPendingPermissionsPerSession: positiveInteger.optional(),
				maxPendingPermissionsPerVm: positiveInteger.optional(),
				maxPermissionOutcomesPerSession: positiveInteger.optional(),
				maxPermissionOutcomesPerVm: positiveInteger.optional(),
			})
			.strict()
			.optional(),
		sqlite: z
			.object({ maxResultBytes: positiveInteger.optional() })
			.strict()
			.optional(),
		jsRuntime: z
			.object({
				v8HeapLimitMb: positiveInteger.optional(),
				syncRpcWaitTimeoutMs: nonNegativeInteger.optional(),
				cpuTimeLimitMs: nonNegativeInteger.optional(),
				wallClockLimitMs: nonNegativeInteger.optional(),
				importCacheMaterializeTimeoutMs: positiveInteger.optional(),
				capturedOutputLimitBytes: positiveInteger.optional(),
				stdinBufferLimitBytes: positiveInteger.optional(),
				eventPayloadLimitBytes: positiveInteger.optional(),
				v8IpcMaxFrameBytes: positiveInteger.optional(),
			})
			.strict()
			.optional(),
		python: z
			.object({
				outputBufferMaxBytes: positiveInteger.optional(),
				executionTimeoutMs: positiveInteger.optional(),
				maxOldSpaceMb: nonNegativeInteger.optional(),
				vfsRpcTimeoutMs: positiveInteger.optional(),
			})
			.strict()
			.optional(),
		wasm: z
			.object({
				maxModuleFileBytes: positiveInteger.optional(),
				capturedOutputLimitBytes: positiveInteger.optional(),
				syncReadLimitBytes: positiveInteger.optional(),
				prewarmTimeoutMs: positiveInteger.optional(),
				runnerHeapLimitMb: positiveInteger.optional(),
				activeCpuTimeLimitMs: nonNegativeInteger.optional(),
				wallClockLimitMs: nonNegativeInteger.optional(),
				deterministicFuel: nonNegativeInteger.optional(),
			})
			.strict()
			.optional(),
		execution: z
			.object({
				completedTtlMs: positiveInteger.optional(),
				maxCompletedExecutions: positiveInteger.optional(),
				liveExecutionWarningThreshold: positiveInteger.optional(),
			})
			.strict()
			.optional(),
		process: z
			.object({
				maxSpawnFileActions: positiveInteger.optional(),
				maxSpawnFileActionBytes: positiveInteger.optional(),
				pendingStdinBytes: positiveInteger.optional(),
				pendingEventCount: positiveInteger.optional(),
				pendingEventBytes: positiveInteger.optional(),
				maxPendingChildSyncCount: positiveInteger.optional(),
				maxPendingChildSyncBytes: positiveInteger.optional(),
			})
			.strict()
			.optional(),
	})
	.strict();

const rootLowerInputSchema = z.union([
	z.object({ kind: z.literal("bundled-base-filesystem") }).strict(),
	z
		.object({ kind: z.literal("snapshot-export"), source: z.unknown() })
		.strict(),
]);

const overlayRootFilesystemConfigSchema = z
	.object({
		type: z.literal("overlay").optional(),
		mode: z.enum(["ephemeral", "read-only"]).optional(),
		disableDefaultBaseLayer: z.boolean().optional(),
		lowers: z.array(rootLowerInputSchema).optional(),
	})
	.strict();

const nativeMountPluginSchema = z
	.object({
		id: z.string(),
		config: z.unknown().optional(),
	})
	.strict();

const nativeRootFilesystemConfigSchema = z
	.object({
		type: z.literal("native"),
		plugin: nativeMountPluginSchema,
		readOnly: z.boolean().optional(),
	})
	.strict();

export const rootFilesystemConfigSchema = z.union([
	overlayRootFilesystemConfigSchema,
	nativeRootFilesystemConfigSchema,
]);

const plainMountConfigSchema = z
	.object({
		path: z.string(),
		driver: z.custom((value) => typeof value === "object" && value !== null, {
			message: "Expected filesystem driver object",
		}),
		guestFstype: z.string().min(1).optional(),
		guestSource: z.string().min(1).optional(),
		readOnly: z.boolean().optional(),
	})
	.strict();

export const nativeMountConfigSchema = z
	.object({
		path: z.string(),
		plugin: nativeMountPluginSchema,
		guestFstype: z.string().min(1).optional(),
		guestSource: z.string().min(1).optional(),
		readOnly: z.boolean().optional(),
	})
	.strict() as z.ZodType<NativeMountConfig>;

const overlayMountConfigSchema = z
	.object({
		path: z.string(),
		filesystem: z
			.object({
				type: z.literal("overlay"),
				store: z.unknown(),
				mode: z.enum(["ephemeral", "read-only"]).optional(),
				lowers: z.array(z.unknown()),
			})
			.strict(),
	})
	.strict();

export const mountConfigSchema = z.union([
	plainMountConfigSchema,
	nativeMountConfigSchema,
	overlayMountConfigSchema,
]);

export const sharedSidecarConfigSchema = z
	.object({
		kind: z.literal("shared"),
		pool: z.string().optional(),
	})
	.strict();

const explicitSidecarSchema = z
	.object({
		kind: z.literal("explicit"),
		handle: z.unknown(),
	})
	.strict();

export const sidecarConfigSchema = z.union([
	sharedSidecarConfigSchema,
	explicitSidecarSchema,
]);

const bindingExampleSchema = z
	.object({
		description: z.string(),
		input: z.unknown(),
	})
	.strict();

export const bindingSchema = z
	.object({
		description: z.string(),
		inputSchema: z.custom(
			(value) => typeof value === "object" && value !== null,
			{
				message: "Expected Zod schema object",
			},
		),
		execute: functionSchema,
		examples: z.array(bindingExampleSchema).optional(),
		timeout: nonNegativeInteger.optional(),
	})
	.strict() as z.ZodType<Binding>;

export const bindingsSchema = z
	.object({
		name: z.string(),
		description: z.string(),
		bindings: z.record(z.string(), bindingSchema),
	})
	.strict() as z.ZodType<Bindings>;

/**
 * Shared AgentOsOptions field schemas.
 *
 * Core and the TypeScript Rivet actor both use the full object. Runtime-owned
 * behavior is serialized by AgentOs.create() into the sidecar VM config.
 */
export const agentOsOptionFieldSchemas = {
	user: vmUserConfigSchema.optional(),
	software: z.array(z.unknown()).optional(),
	defaultSoftware: z.boolean().optional(),
	loopbackExemptPorts: z.array(z.number().int().min(0).max(65535)).optional(),
	allowedNodeBuiltins: stringArray.optional(),
	highResolutionTime: z.boolean().optional(),
	database: z
		.discriminatedUnion("type", [
			z
				.object({
					type: z.literal("actor_uds"),
					path: z.string().min(1),
				})
				.strict(),
			z
				.object({
					type: z.literal("sqlite_file"),
					path: z.string().min(1),
				})
				.strict(),
		])
		.optional(),
	rootFilesystem: rootFilesystemConfigSchema.optional(),
	mounts: z.array(mountConfigSchema).optional(),
	sandbox: z
		.custom(
			(value) =>
				value === undefined ||
				(typeof value === "object" && value !== null && !Array.isArray(value)),
			{ message: "Expected sandbox options object" },
		)
		.optional(),
	scheduleDriver: z
		.custom((value) => typeof value === "object" && value !== null, {
			message: "Expected schedule driver object",
		})
		.optional(),
	bindings: z.array(bindingsSchema).optional(),
	permissions: permissionsSchema.optional(),
	sidecar: sidecarConfigSchema.optional(),
	limits: agentOsLimitsSchema.optional(),
	onAgentStderr: z
		.custom<AgentStderrHandler>((value) => typeof value === "function", {
			message: "Expected function",
		})
		.optional(),
	onAgentExit: z
		.custom<AgentExitHandler>((value) => typeof value === "function", {
			message: "Expected function",
		})
		.optional(),
	onLimitWarning: z
		.custom<LimitWarningHandler>((value) => typeof value === "function", {
			message: "Expected function",
		})
		.optional(),
} as const;

export const agentOsOptionsSchema = z
	.object(agentOsOptionFieldSchemas)
	.strict() as z.ZodType<AgentOsOptions>;

export function parseAgentOsOptions(options?: AgentOsOptions): AgentOsOptions {
	return agentOsOptionsSchema.parse(options ?? {});
}
