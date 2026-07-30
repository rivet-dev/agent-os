import * as protocol from "./generated-protocol.js";
import {
	fromGeneratedRootFilesystemEntryEncoding,
	fromGeneratedRootFilesystemEntryKind,
	toGeneratedRootFilesystemEntryEncoding,
	toGeneratedRootFilesystemEntryKind,
	toGeneratedRootFilesystemMode,
	type LiveRootFilesystemEntryEncoding,
} from "./protocol-maps.js";

export type { LiveRootFilesystemEntryEncoding } from "./protocol-maps.js";

export type GuestFilesystemContentEncoding = "utf8" | "base64";

export interface GuestFilesystemContentResult {
	path: string;
	content?: string;
	encoding?: GuestFilesystemContentEncoding;
}

export type LiveRootFilesystemEntry = {
	path: string;
	kind: "file" | "directory" | "symlink";
	mode?: number;
	uid?: number;
	gid?: number;
	content?: string;
	encoding?: LiveRootFilesystemEntryEncoding;
	target?: string;
	executable?: boolean;
};

export type LiveRootFilesystemLowerDescriptor =
	| {
			kind: "snapshot";
			entries: LiveRootFilesystemEntry[];
	  }
	| {
			kind: "bundled_base_filesystem";
	  };

export type LiveRootFilesystemDescriptor = {
	mode?: "ephemeral" | "read_only";
	disable_default_base_layer?: boolean;
	lowers?: LiveRootFilesystemLowerDescriptor[];
	bootstrap_entries?: LiveRootFilesystemEntry[];
};

export function encodeGuestFilesystemContent(content: string | Uint8Array): {
	content: string;
	encoding?: GuestFilesystemContentEncoding;
} {
	if (typeof content === "string") {
		return { content };
	}

	return {
		content: Buffer.from(content).toString("base64"),
		encoding: "base64",
	};
}

export function decodeGuestFilesystemContent(
	response: GuestFilesystemContentResult,
): Uint8Array {
	if (response.content === undefined) {
		throw new Error(`sidecar returned no file content for ${response.path}`);
	}

	if (response.encoding === "base64") {
		return Buffer.from(response.content, "base64");
	}

	return Buffer.from(response.content, "utf8");
}

export function toGeneratedRootFilesystemDescriptor(
	descriptor: LiveRootFilesystemDescriptor,
): protocol.RootFilesystemDescriptor {
	return {
		mode: toGeneratedRootFilesystemMode(descriptor.mode ?? "ephemeral"),
		disableDefaultBaseLayer: descriptor.disable_default_base_layer ?? false,
		lowers: (descriptor.lowers ?? []).map(toGeneratedRootFilesystemLower),
		bootstrapEntries: (descriptor.bootstrap_entries ?? []).map(
			toGeneratedRootFilesystemEntry,
		),
	};
}

export function toGeneratedRootFilesystemLower(
	lower: LiveRootFilesystemLowerDescriptor,
): protocol.RootFilesystemLowerDescriptor {
	switch (lower.kind) {
		case "snapshot":
			return {
				tag: "SnapshotRootFilesystemLower",
				val: {
					entries: (lower.entries ?? []).map(toGeneratedRootFilesystemEntry),
				},
			};
		case "bundled_base_filesystem":
			return { tag: "BundledBaseFilesystemLower", val: null };
	}
}

export function toGeneratedRootFilesystemEntry(
	entry: LiveRootFilesystemEntry,
): protocol.RootFilesystemEntry {
	return {
		path: entry.path,
		kind: toGeneratedRootFilesystemEntryKind(entry.kind),
		mode: entry.mode ?? null,
		uid: entry.uid ?? null,
		gid: entry.gid ?? null,
		content: entry.content ?? null,
		encoding:
			entry.encoding === undefined
				? null
				: toGeneratedRootFilesystemEntryEncoding(entry.encoding),
		target: entry.target ?? null,
		executable: entry.executable ?? false,
	};
}

export function fromGeneratedRootFilesystemEntry(
	entry: protocol.RootFilesystemEntry,
): LiveRootFilesystemEntry {
	return {
		path: entry.path,
		kind: fromGeneratedRootFilesystemEntryKind(entry.kind),
		...(entry.mode !== null ? { mode: entry.mode } : {}),
		...(entry.uid !== null ? { uid: entry.uid } : {}),
		...(entry.gid !== null ? { gid: entry.gid } : {}),
		...(entry.content !== null ? { content: entry.content } : {}),
		...(entry.encoding !== null
			? { encoding: fromGeneratedRootFilesystemEntryEncoding(entry.encoding) }
			: {}),
		...(entry.target !== null ? { target: entry.target } : {}),
		executable: entry.executable,
	};
}
