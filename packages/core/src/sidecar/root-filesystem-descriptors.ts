import { getBaseFilesystemEntries } from "../base-filesystem.js";
import type { RootFilesystemConfig, RootLowerInput } from "../agent-os.js";
import type { FilesystemEntry } from "../filesystem-snapshot.js";
import type { RootSnapshotExport } from "../layers.js";

export interface SidecarRootFilesystemDescriptor {
	mode: "ephemeral" | "read_only";
	disableDefaultBaseLayer: boolean;
	lowers: SidecarRootFilesystemLowerDescriptor[];
	bootstrapEntries: SidecarRootFilesystemEntry[];
}

export interface SidecarRootFilesystemLowerDescriptor {
	kind: "snapshot";
	entries: SidecarRootFilesystemEntry[];
}

export interface SidecarRootFilesystemEntry {
	path: string;
	kind: "file" | "directory" | "symlink";
	mode?: number;
	uid?: number;
	gid?: number;
	content?: string;
	encoding?: "utf8" | "base64";
	target?: string;
	executable: boolean;
}

export function serializeRootFilesystemForSidecar(
	config?: RootFilesystemConfig,
	bootstrapLower?: RootSnapshotExport | null,
): SidecarRootFilesystemDescriptor {
	const lowerInputs = [...(config?.lowers ?? []), ...(bootstrapLower ? [bootstrapLower] : [])];

	return {
		mode: config?.mode === "read-only" ? "read_only" : "ephemeral",
		disableDefaultBaseLayer: config?.disableDefaultBaseLayer ?? false,
		lowers: lowerInputs.map(serializeRootLowerForSidecar),
		bootstrapEntries: [],
	};
}

function serializeRootLowerForSidecar(
	lower: RootLowerInput,
): SidecarRootFilesystemLowerDescriptor {
	if (lower.kind === "bundled-base-filesystem") {
		return {
			kind: "snapshot",
			entries: getBaseFilesystemEntries().map(serializeFilesystemEntryForSidecar),
		};
	}

	return {
		kind: "snapshot",
		entries: lower.source.filesystem.entries.map(serializeFilesystemEntryForSidecar),
	};
}

function serializeFilesystemEntryForSidecar(
	entry: FilesystemEntry,
): SidecarRootFilesystemEntry {
	const mode = Number.parseInt(entry.mode, 8);
	return {
		path: entry.path,
		kind: entry.type,
		mode,
		uid: entry.uid,
		gid: entry.gid,
		content: entry.content,
		encoding: entry.encoding,
		target: entry.target,
		executable: entry.type === "file" && (mode & 0o111) !== 0,
	};
}
