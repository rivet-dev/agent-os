import type {
	NativeMountConfig,
	PlainMountConfig,
} from "../agent-os.js";

export type MountConfigJsonValue =
	| string
	| number
	| boolean
	| null
	| MountConfigJsonObject
	| MountConfigJsonValue[];

export interface MountConfigJsonObject {
	[key: string]: MountConfigJsonValue;
}

export interface SidecarMountPluginDescriptor {
	id: string;
	config: MountConfigJsonObject;
}

export interface SidecarMountDescriptor {
	guestPath: string;
	readOnly: boolean;
	plugin: SidecarMountPluginDescriptor;
}

export function serializeMountConfigForSidecar(
	mount: PlainMountConfig | NativeMountConfig,
): SidecarMountDescriptor {
	if ("driver" in mount) {
		return {
			guestPath: mount.path,
			readOnly: mount.readOnly ?? false,
			plugin: {
				id: "js_bridge",
				config: {},
			},
		};
	}

	return {
		guestPath: mount.path,
		readOnly: mount.readOnly ?? false,
		plugin: {
			id: mount.plugin.id,
			config: mount.plugin.config ?? {},
		},
	};
}
