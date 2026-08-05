import type * as protocol from "./generated-protocol.js";
import { stringifyJsonUtf8 } from "./json.js";

export type LiveSidecarPlacement =
	| { kind: "shared"; pool?: string | null }
	| { kind: "explicit"; sidecar_id: string };

export type MountConfigJsonPrimitive = string | number | boolean | null;
export type MountConfigJsonValue =
	| MountConfigJsonPrimitive
	| MountConfigJsonObject
	| MountConfigJsonValue[];

export interface MountConfigJsonObject {
	[key: string]: MountConfigJsonValue;
}

export interface NativeMountPluginDescriptor<
	TConfig extends MountConfigJsonObject = MountConfigJsonObject,
> {
	id: string;
	config?: TConfig;
}

export interface S3MountCredentialsConfig {
	accessKeyId: string;
	secretAccessKey: string;
}

// Temporarily disabled; keep this API source in place for a deliberate return.
// `object_s3` stores each file as one object, so ordinary POSIX metadata updates,
// sparse writes, truncates, and random writes can become whole-object downloads
// and replacements. During strict filesystem validation, one applicable pathname
// test issued 3,992,055 S3 requests and exceeded a two-hour bound. Coalescing those
// writes requires a bounded, coherent dirty-inode cache with correct behavior for
// reads, links, renames, unlinks, fsync/sync, eviction, unmount, and upload errors;
// exposing the descriptor before all of those durability and errno paths are
// proven would advertise a filesystem whose correctness contract is incomplete.
// Bring this back only when the native plugin is re-registered and its focused
// request-count, flush-failure, and applicable pinned xfstests coverage is green.
// export interface ObjectS3MountConfig {
// 	bucket: string;
// 	prefix?: string;
// 	region?: string;
// 	credentials?: S3MountCredentialsConfig;
// 	endpoint?: string;
// 	uid?: number;
// 	gid?: number;
// 	fileMode?: number;
// 	dirMode?: number;
// }

export interface ChunkedS3MountConfig {
	bucket: string;
	prefix?: string;
	region?: string;
	credentials?: S3MountCredentialsConfig;
	endpoint?: string;
	metadataBackend?: "sqlite" | "local" | "callback";
	metadataPath?: string;
	mountId?: string;
	chunkSize?: number;
	inlineThreshold?: number;
	uid?: number;
	gid?: number;
	fileMode?: number;
	dirMode?: number;
	metadataCacheEntries?: number;
}

export interface ChunkedLocalMountConfig {
	metadataPath: string;
	blockRoot: string;
	chunkSize?: number;
	inlineThreshold?: number;
	uid?: number;
	gid?: number;
	fileMode?: number;
	dirMode?: number;
	metadataCacheEntries?: number;
}

// export function objectS3MountPlugin(
// 	config: ObjectS3MountConfig,
// ): NativeMountPluginDescriptor {
// 	return {
// 		id: "object_s3",
// 		config: config as unknown as MountConfigJsonObject,
// 	};
// }

export function chunkedS3MountPlugin(
	config: ChunkedS3MountConfig,
): NativeMountPluginDescriptor {
	return {
		id: "chunked_s3",
		config: config as unknown as MountConfigJsonObject,
	};
}

export function chunkedLocalMountPlugin(
	config: ChunkedLocalMountConfig,
): NativeMountPluginDescriptor {
	return {
		id: "chunked_local",
		config: config as unknown as MountConfigJsonObject,
	};
}

export interface LiveMountDescriptor {
	guest_path: string;
	guest_source?: string;
	guest_fstype?: string;
	read_only: boolean;
	plugin: NativeMountPluginDescriptor;
}

export interface LiveSoftwareDescriptor {
	package_name: string;
	root: string;
}

export interface LiveProjectedModuleDescriptor {
	package_name: string;
	entrypoint: string;
}

export interface LivePackageDescriptor {
	path: string;
}

export function toGeneratedSidecarPlacement(
	placement: LiveSidecarPlacement,
): protocol.SidecarPlacement {
	switch (placement.kind) {
		case "shared":
			return {
				tag: "SidecarPlacementShared",
				val: { pool: placement.pool ?? null },
			};
		case "explicit":
			return {
				tag: "SidecarPlacementExplicit",
				val: { sidecarId: placement.sidecar_id },
			};
	}
}

export function toGeneratedMountDescriptor(
	descriptor: LiveMountDescriptor,
): protocol.MountDescriptor {
	return {
		guestPath: descriptor.guest_path,
		guestSource: descriptor.guest_source ?? descriptor.plugin.id,
		guestFstype: descriptor.guest_fstype ?? descriptor.plugin.id,
		readOnly: descriptor.read_only,
		plugin: {
			id: descriptor.plugin.id,
			config: stringifyJsonUtf8(
				descriptor.plugin.config ?? {},
				"mount plugin config",
			),
		},
	};
}

export function toGeneratedSoftwareDescriptor(
	descriptor: LiveSoftwareDescriptor,
): protocol.SoftwareDescriptor {
	return {
		packageName: descriptor.package_name,
		root: descriptor.root,
	};
}

export function toGeneratedProjectedModuleDescriptor(
	descriptor: LiveProjectedModuleDescriptor,
): protocol.ProjectedModuleDescriptor {
	return {
		packageName: descriptor.package_name,
		entrypoint: descriptor.entrypoint,
	};
}

export function toGeneratedPackageDescriptor(
	descriptor: LivePackageDescriptor,
): protocol.PackageDescriptor {
	return {
		path: descriptor.path,
	};
}
