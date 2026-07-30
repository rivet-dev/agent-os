import type {
	MountConfigJsonObject,
	NativeMountPluginDescriptor,
} from "./descriptors.js";

export interface HostDirBackendOptions {
	/** Absolute path to the host directory to project into the VM. */
	hostPath: string;
	/** If true (default), write operations are blocked for the mount. */
	readOnly?: boolean;
}

export interface HostDirMountPluginConfig extends MountConfigJsonObject {
	hostPath: string;
	readOnly: boolean;
}

/**
 * Create a declarative host-dir mount plugin descriptor.
 *
 * This keeps the legacy helper name while routing first-party host-dir
 * mounts through the native `host_dir` plugin instead of a JS VFS backend.
 */
export function createHostDirBackend(
	options: HostDirBackendOptions,
): NativeMountPluginDescriptor<HostDirMountPluginConfig> {
	return {
		id: "host_dir",
		config: {
			hostPath: options.hostPath,
			readOnly: options.readOnly ?? true,
		},
	};
}

/** A native `host_dir` mount, the serializable form `AgentOsOptions.mounts` accepts. */
export interface NodeModulesMountConfig {
	path: string;
	plugin: NativeMountPluginDescriptor<HostDirMountPluginConfig>;
	readOnly: boolean;
}

/**
 * Mount a host `node_modules` directory into the VM at `/root/node_modules`.
 *
 * This is the explicit replacement for the removed `moduleAccessCwd` option:
 * the VM's module resolver reads the mounted tree through the kernel VFS, so the
 * caller supplies exactly the `node_modules` directory whose packages should be
 * resolvable in the guest (e.g. the agent SDK + its transitive deps).
 *
 * @param hostNodeModulesDir Absolute host path to a `node_modules` directory.
 * @param opts.readOnly Defaults to `true`; the mount is read-only.
 */
export function nodeModulesMount(
	hostNodeModulesDir: string,
	opts?: { readOnly?: boolean },
): NodeModulesMountConfig {
	const readOnly = opts?.readOnly ?? true;
	return {
		path: "/root/node_modules",
		plugin: createHostDirBackend({ hostPath: hostNodeModulesDir, readOnly }),
		readOnly,
	};
}
