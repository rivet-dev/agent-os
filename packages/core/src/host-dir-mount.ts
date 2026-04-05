import type {
	MountConfigJsonObject,
	NativeMountPluginDescriptor,
} from "./agent-os.js";

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
