import { z } from "zod";
import type { NodeRuntimeCreateOptions } from "./node-runtime.js";

const permissionModeSchema = z.enum(["allow", "deny"]);
const stringArray = z.array(z.string());
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

export const nodeRuntimePermissionsSchema = z
	.object({
		fs: fsPermissionsSchema.optional(),
		network: patternPermissionsSchema.optional(),
		childProcess: patternPermissionsSchema.optional(),
		process: patternPermissionsSchema.optional(),
		env: patternPermissionsSchema.optional(),
		binding: patternPermissionsSchema.optional(),
	})
	.strict();

const uint8ArraySchema = z.custom<Uint8Array>(
	(value: unknown) => value instanceof Uint8Array,
	{ message: "Expected Uint8Array" },
);

const hostDirectoryMountSchema = z
	.object({
		guestPath: z.string(),
		hostPath: z.string(),
		readOnly: z.boolean().optional(),
	})
	.strict();

const nodeModulesMountSchema = z
	.object({
		hostPath: z.string(),
		guestPath: z.string().optional(),
	})
	.strict();

const jsRuntimeSchema = z
	.object({
		platform: z.enum(["node", "browser", "neutral", "bare"]).optional(),
		moduleResolution: z.enum(["node", "relative", "none"]).optional(),
		allowedBuiltins: stringArray.optional(),
		highResolutionTime: z.boolean().optional(),
	})
	.strict();

const bindingExampleSchema = z
	.object({
		description: z.string(),
		input: z.unknown(),
	})
	.strict();

const bindingDefinitionSchema = z
	.object({
		description: z.string(),
		inputSchema: z.custom<object>(
			(value: unknown) => typeof value === "object" && value !== null,
			{ message: "Expected JSON Schema object" },
		),
		timeoutMs: z.number().int().nonnegative().optional(),
		examples: z.array(bindingExampleSchema).optional(),
		commandAliases: stringArray.optional(),
		handler: z.custom<(input: unknown) => unknown | Promise<unknown>>(
			(value: unknown) => typeof value === "function",
			{ message: "Expected function" },
		),
	})
	.strict();

/**
 * Runtime validation for the public `NodeRuntime.create(...)` API.
 *
 * This is the TS-side guard for the ergonomic options shape. The sidecar VM
 * JSON it eventually produces is still validated by
 * `crates/vm-config/src/lib.rs::CreateVmConfig` with `deny_unknown_fields`.
 * Keep these in sync when adding high-level create options that translate into
 * the Rust VM config.
 */
export const nodeRuntimeCreateOptionsSchema = z
	.object({
		filesystem: z.custom<NodeRuntimeCreateOptions["filesystem"]>(
			(value: unknown) => typeof value === "object" && value !== null,
			{ message: "Expected caller-owned VirtualFileSystem object" },
		),
		env: z.record(z.string(), z.string()).optional(),
		cwd: z.string().optional(),
		user: vmUserConfigSchema.optional(),
		permissions: nodeRuntimePermissionsSchema.optional(),
		commandsDir: z.string().optional(),
		wasmCommandDirs: stringArray.optional(),
		sidecar: z
			.custom((value: unknown) => typeof value === "object" && value !== null, {
				message: "Expected SidecarProcess object",
			})
			.optional(),
		onBootTiming: z
			.custom<(timing: unknown) => void>(
				(value: unknown) => typeof value === "function",
				{ message: "Expected function" },
			)
			.optional(),
		files: z
			.record(z.string(), z.union([z.string(), uint8ArraySchema]))
			.optional(),
		mounts: z.array(hostDirectoryMountSchema).optional(),
		nodeModules: z.union([z.string(), nodeModulesMountSchema]).optional(),
		bindings: z.record(z.string(), bindingDefinitionSchema).optional(),
		loopbackExemptPorts: z.array(z.number().int().min(0).max(65535)).optional(),
		jsRuntime: jsRuntimeSchema.optional(),
	})
	.strict() as z.ZodType<NodeRuntimeCreateOptions>;

export function parseNodeRuntimeCreateOptions(
	options: NodeRuntimeCreateOptions,
): NodeRuntimeCreateOptions {
	return nodeRuntimeCreateOptionsSchema.parse(options);
}
