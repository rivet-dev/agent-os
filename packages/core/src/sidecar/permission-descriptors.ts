import type {
	NetworkAccessRequest,
	PermissionDecision,
	Permissions,
} from "../runtime-compat.js";
import type { SidecarPermissionDescriptor } from "./native-process-client.js";

type SidecarPermissionMode = SidecarPermissionDescriptor["mode"];

interface FsPermissionSample {
	capability: string;
	requests: Array<{ path: string; operation: string }>;
}

interface NetworkPermissionSample {
	capability: string;
	requests: NetworkAccessRequest[];
}

const DEFAULT_SIDE_CAR_PERMISSIONS: SidecarPermissionDescriptor[] = [
	{ capability: "fs", mode: "allow" },
	{ capability: "network", mode: "allow" },
	{ capability: "child_process", mode: "allow" },
	{ capability: "env", mode: "allow" },
];

const FS_PERMISSION_SAMPLES: FsPermissionSample[] = [
	{
		capability: "fs.read",
		requests: [
			{ path: "/workspace/policy-probe.txt", operation: "read" },
			{ path: "/tmp/policy-probe.txt", operation: "read" },
		],
	},
	{
		capability: "fs.write",
		requests: [
			{ path: "/workspace/policy-probe.txt", operation: "write" },
			{ path: "/tmp/policy-probe.txt", operation: "write" },
		],
	},
	{
		capability: "fs.create_dir",
		requests: [
			{ path: "/workspace/policy-probe-dir", operation: "mkdir" },
			{ path: "/tmp/policy-probe-dir", operation: "mkdir" },
		],
	},
	{
		capability: "fs.create_dir",
		requests: [
			{ path: "/workspace/policy-probe-dir", operation: "createDir" },
			{ path: "/tmp/policy-probe-dir", operation: "createDir" },
		],
	},
	{
		capability: "fs.readdir",
		requests: [
			{ path: "/workspace", operation: "readdir" },
			{ path: "/tmp", operation: "readdir" },
		],
	},
	{
		capability: "fs.stat",
		requests: [
			{ path: "/workspace/policy-probe.txt", operation: "stat" },
			{ path: "/tmp/policy-probe.txt", operation: "stat" },
		],
	},
	{
		capability: "fs.rm",
		requests: [
			{ path: "/workspace/policy-probe.txt", operation: "rm" },
			{ path: "/tmp/policy-probe.txt", operation: "rm" },
		],
	},
	{
		capability: "fs.rename",
		requests: [
			{ path: "/workspace/policy-probe.txt", operation: "rename" },
			{ path: "/tmp/policy-probe.txt", operation: "rename" },
		],
	},
	{
		capability: "fs.stat",
		requests: [
			{ path: "/workspace/policy-probe.txt", operation: "exists" },
			{ path: "/tmp/policy-probe.txt", operation: "exists" },
		],
	},
	{
		capability: "fs.symlink",
		requests: [
			{ path: "/workspace/policy-probe-link.txt", operation: "symlink" },
			{ path: "/tmp/policy-probe-link.txt", operation: "symlink" },
		],
	},
	{
		capability: "fs.readlink",
		requests: [
			{ path: "/workspace/policy-probe-link.txt", operation: "readlink" },
			{ path: "/tmp/policy-probe-link.txt", operation: "readlink" },
		],
	},
	{
		capability: "fs.write",
		requests: [
			{ path: "/workspace/policy-probe.txt", operation: "link" },
			{ path: "/tmp/policy-probe.txt", operation: "link" },
		],
	},
	{
		capability: "fs.chmod",
		requests: [
			{ path: "/workspace/policy-probe.txt", operation: "chmod" },
			{ path: "/tmp/policy-probe.txt", operation: "chmod" },
		],
	},
	{
		capability: "fs.write",
		requests: [
			{ path: "/workspace/policy-probe.txt", operation: "chown" },
			{ path: "/tmp/policy-probe.txt", operation: "chown" },
		],
	},
	{
		capability: "fs.write",
		requests: [
			{ path: "/workspace/policy-probe.txt", operation: "utimes" },
			{ path: "/tmp/policy-probe.txt", operation: "utimes" },
		],
	},
	{
		capability: "fs.truncate",
		requests: [
			{ path: "/workspace/policy-probe.txt", operation: "truncate" },
			{ path: "/tmp/policy-probe.txt", operation: "truncate" },
		],
	},
	{
		capability: "fs.mount_sensitive",
		requests: [
			{ path: "/etc", operation: "mountSensitive" },
			{ path: "/proc", operation: "mountSensitive" },
		],
	},
] as const;

const NETWORK_PERMISSION_SAMPLES: NetworkPermissionSample[] = [
	{
		capability: "network.fetch",
		requests: [
			{
				url: "https://example.test/fetch",
				host: "example.test",
				port: 443,
				protocol: "https",
			},
			{
				url: "http://127.0.0.1:4318/fetch",
				host: "127.0.0.1",
				port: 4318,
				protocol: "http",
			},
		],
	},
	{
		capability: "network.http",
		requests: [
			{
				url: "https://example.test/http",
				host: "example.test",
				port: 443,
				protocol: "https",
			},
			{
				url: "http://127.0.0.1:4318/http",
				host: "127.0.0.1",
				port: 4318,
				protocol: "http",
			},
		],
	},
	{
		capability: "network.dns",
		requests: [
			{ host: "example.test", protocol: "dns" },
			{ host: "localhost", protocol: "dns" },
		],
	},
	{
		capability: "network.listen",
		requests: [
			{ host: "127.0.0.1", port: 3000, protocol: "tcp" },
			{ host: "0.0.0.0", port: 3001, protocol: "tcp" },
		],
	},
] as const;

function normalizeDecision(decision: PermissionDecision): SidecarPermissionMode {
	if (typeof decision === "boolean") {
		return decision ? "allow" : "deny";
	}
	return decision.allowed ? "allow" : "deny";
}

function inferUniformMode<T>(
	label: string,
	check: ((request: T) => PermissionDecision) | undefined,
	requests: readonly T[],
): SidecarPermissionMode | null {
	if (!check) {
		return null;
	}
	const [firstRequest, ...rest] = requests;
	if (firstRequest === undefined) {
		return null;
	}
	const mode = normalizeDecision(check(firstRequest));
	for (const request of rest) {
		if (normalizeDecision(check(request)) !== mode) {
			throw new Error(
				`${label} permission callback varies by resource and cannot be serialized for the native sidecar`,
			);
		}
	}
	return mode;
}

function inferFsDescriptors(
	permissions: NonNullable<Permissions["fs"]>,
): SidecarPermissionDescriptor[] {
	const descriptorModes = new Map<string, SidecarPermissionMode>();
	for (const sample of FS_PERMISSION_SAMPLES) {
		const mode = inferUniformMode(sample.capability, permissions, sample.requests);
		if (!mode) {
			continue;
		}
		const existingMode = descriptorModes.get(sample.capability);
		if (existingMode && existingMode !== mode) {
			throw new Error(
				`${sample.capability} permission callback varies by operation and cannot be serialized for the native sidecar`,
			);
		}
		descriptorModes.set(sample.capability, mode);
	}
	const descriptors = [...descriptorModes.entries()].map(([capability, mode]) => ({
		capability,
		mode,
	}));

	if (descriptors.length === 0) {
		return [];
	}

	const [firstDescriptor, ...rest] = descriptors;
	if (
		firstDescriptor &&
		rest.every((descriptor) => descriptor.mode === firstDescriptor.mode)
	) {
		return [{ capability: "fs", mode: firstDescriptor.mode }];
	}

	return descriptors;
}

function inferNetworkDescriptors(
	permissions: NonNullable<Permissions["network"]>,
): SidecarPermissionDescriptor[] {
	const descriptors = NETWORK_PERMISSION_SAMPLES.map((sample) => ({
		capability: sample.capability,
		mode: inferUniformMode(sample.capability, permissions, sample.requests),
	})).filter(
		(
			descriptor,
		): descriptor is SidecarPermissionDescriptor & {
			mode: SidecarPermissionMode;
		} => descriptor.mode !== null,
	);

	if (descriptors.length === 0) {
		return [];
	}

	const [firstDescriptor, ...rest] = descriptors;
	if (
		firstDescriptor &&
		rest.every((descriptor) => descriptor.mode === firstDescriptor.mode)
	) {
		return [{ capability: "network", mode: firstDescriptor.mode }];
	}

	return descriptors;
}

export function serializePermissionsForSidecar(
	permissions?: Permissions,
): SidecarPermissionDescriptor[] {
	if (permissions === undefined) {
		return [...DEFAULT_SIDE_CAR_PERMISSIONS];
	}

	const descriptors: SidecarPermissionDescriptor[] = [];

	if (permissions.fs) {
		descriptors.push(...inferFsDescriptors(permissions.fs));
	} else {
		descriptors.push({ capability: "fs", mode: "allow" });
	}

	if (permissions.network) {
		descriptors.push(...inferNetworkDescriptors(permissions.network));
	} else {
		descriptors.push({ capability: "network", mode: "allow" });
	}

	if (permissions.childProcess) {
		const mode = inferUniformMode(
			"child_process",
			permissions.childProcess,
			[
				{ command: "node", args: ["-v"] },
				{ command: "bash", args: ["-lc", "true"] },
			],
		);
		if (mode) {
			descriptors.push({ capability: "child_process", mode });
		}
	} else {
		descriptors.push({ capability: "child_process", mode: "allow" });
	}

	if (permissions.env) {
		const mode = inferUniformMode("env.read", permissions.env, [
			{ name: "HOME", value: "/home/user" },
			{ name: "SECRET_KEY", value: "hidden" },
		]);
		if (mode) {
			descriptors.push({ capability: "env", mode });
		}
	} else {
		descriptors.push({ capability: "env", mode: "deny" });
	}

	return descriptors;
}
