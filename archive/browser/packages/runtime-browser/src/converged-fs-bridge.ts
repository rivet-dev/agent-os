// Converged filesystem bridge translation layer.
//
// The legacy browser executor serviced guest `fs.*` sync-bridge operations
// against an in-process TypeScript kernel (`runtime-driver.ts`'s
// `handleSyncBridgeOperation`). The converged executor instead routes every
// guest filesystem syscall to the wasm sidecar (`archive/browser/crates/sidecar-browser`) over
// the wire protocol, so the kernel is the single enforcement point on both
// native and browser.
//
// This module is the pure, backend-agnostic translation between a sync-bridge
// `fs.*` operation (the guest-facing shape in `worker.ts`) and the wire
// `guest_filesystem_call` request / `guest_filesystem_result` response. It is
// unit-tested in Node without a wasm sidecar or a worker; the handler that owns
// a `BrowserSidecarWasm` instance (and the multi-call `readDir`-with-types
// expansion) builds on top of it.

import type {
	LiveGuestFilesystemStat,
	LiveRequestPayload,
	LiveResponsePayload,
} from "@rivet-dev/agentos-runtime-core";
import type { VirtualDirEntry, VirtualStat } from "./runtime.js";
import { decodeBase64, encodeBase64 } from "./converged-base64.js";
import {
	SYNC_BRIDGE_KIND_BINARY,
	SYNC_BRIDGE_KIND_JSON,
	SYNC_BRIDGE_KIND_NONE,
	SYNC_BRIDGE_KIND_TEXT,
} from "./sync-bridge.js";

/** The wire `guest_filesystem_call` request payload. */
export type GuestFilesystemRequestPayload = Extract<
	LiveRequestPayload,
	{ type: "guest_filesystem_call" }
>;

/** The wire `guest_filesystem_result` fields the response mapper consumes. */
export type GuestFilesystemResult = Extract<
	LiveResponsePayload,
	{ type: "guest_filesystem_result" }
>;

/** The shape `runtime-driver` writes back to the sync-bridge SAB. */
export type ConvergedSyncResponse =
	| { kind: typeof SYNC_BRIDGE_KIND_NONE }
	| { kind: typeof SYNC_BRIDGE_KIND_TEXT; value: string }
	| { kind: typeof SYNC_BRIDGE_KIND_BINARY; value: Uint8Array }
	| { kind: typeof SYNC_BRIDGE_KIND_JSON; value: unknown };

/** Operations serviced by a single wire `guest_filesystem_call` round-trip. */
const SINGLE_CALL_FS_OPERATIONS = new Set([
	"fs.readFile",
	"fs.writeFile",
	"fs.readFileBinary",
	"fs.writeFileBinary",
	"fs.pread",
	"fs.pwrite",
	"fs.createDir",
	"fs.mkdir",
	"fs.rmdir",
	"fs.exists",
	"fs.stat",
	"fs.lstat",
	"fs.unlink",
	"fs.rename",
	"fs.realpath",
	"fs.readlink",
	"fs.symlink",
	"fs.link",
	"fs.chmod",
	"fs.truncate",
]);

export function isSingleCallFilesystemOperation(operation: string): boolean {
	return SINGLE_CALL_FS_OPERATIONS.has(operation);
}

/**
 * Translate a sync-bridge `fs.*` operation + args into the wire
 * `guest_filesystem_call` request payload. Throws for operations that are not a
 * single-call mapping (e.g. `fs.readDir`, which the handler expands with
 * per-entry `lstat` to recover Dirent types the wire does not carry).
 */
export function convergedFilesystemRequestPayload(
	operation: string,
	args: readonly unknown[],
): GuestFilesystemRequestPayload {
	const path = String(args[0]);
	switch (operation) {
		case "fs.readFile":
		case "fs.readFileBinary":
			return { type: "guest_filesystem_call", operation: "read_file", path };
		case "fs.pread":
			return {
				type: "guest_filesystem_call",
				operation: "pread",
				path,
				offset: Number(args[1] ?? 0),
				len: Number(args[2] ?? 0),
			};
		case "fs.writeFile":
			return {
				type: "guest_filesystem_call",
				operation: "write_file",
				path,
				content: String(args[1] ?? ""),
				encoding: "utf8",
			};
		case "fs.writeFileBinary":
			return {
				type: "guest_filesystem_call",
				operation: "write_file",
				path,
				content: encodeBase64(toUint8Array(args[1])),
				encoding: "base64",
			};
		case "fs.pwrite":
			// Positional write: the kernel writes `content` at `offset`, growing
			// and zero-filling the file as needed. This replaces a lossy,
			// non-atomic client-side read-modify-write with a single
			// kernel-enforced call.
			return {
				type: "guest_filesystem_call",
				operation: "pwrite",
				path,
				offset: Number(args[1] ?? 0),
				content: encodeBase64(toUint8Array(args[2])),
				encoding: "base64",
			};
		case "fs.createDir":
			return { type: "guest_filesystem_call", operation: "create_dir", path };
		case "fs.mkdir":
			return {
				type: "guest_filesystem_call",
				operation: "mkdir",
				path,
				recursive: true,
			};
		case "fs.rmdir":
			return { type: "guest_filesystem_call", operation: "remove_dir", path };
		case "fs.exists":
			return { type: "guest_filesystem_call", operation: "exists", path };
		case "fs.stat":
			return { type: "guest_filesystem_call", operation: "stat", path };
		case "fs.lstat":
			return { type: "guest_filesystem_call", operation: "lstat", path };
		case "fs.unlink":
			return { type: "guest_filesystem_call", operation: "remove_file", path };
		case "fs.rename":
			return {
				type: "guest_filesystem_call",
				operation: "rename",
				path,
				destination_path: String(args[1]),
			};
		case "fs.realpath":
			return { type: "guest_filesystem_call", operation: "realpath", path };
		case "fs.readlink":
			return { type: "guest_filesystem_call", operation: "read_link", path };
		case "fs.symlink":
			// Legacy arg order is (target, linkPath); the wire `path` is the link.
			return {
				type: "guest_filesystem_call",
				operation: "symlink",
				path: String(args[1]),
				target: String(args[0]),
			};
		case "fs.link":
			return {
				type: "guest_filesystem_call",
				operation: "link",
				path,
				destination_path: String(args[1]),
			};
		case "fs.chmod":
			return {
				type: "guest_filesystem_call",
				operation: "chmod",
				path,
				mode: Number(args[1]),
			};
		case "fs.truncate":
			return {
				type: "guest_filesystem_call",
				operation: "truncate",
				path,
				len: Number(args[1]),
			};
		default:
			throw new Error(
				`converged filesystem bridge has no single-call mapping for ${operation}`,
			);
	}
}

/**
 * Translate a wire `guest_filesystem_result` back into the sync-bridge response
 * the guest expects for `operation`. `fs.readDir` is intentionally NOT handled
 * here (it requires multiple wire calls); the handler maps it separately.
 */
export function convergedFilesystemSyncResponse(
	operation: string,
	result: GuestFilesystemResult,
): ConvergedSyncResponse {
	switch (operation) {
		case "fs.readFile":
			return {
				kind: SYNC_BRIDGE_KIND_TEXT,
				value: decodeTextContent(result),
			};
		case "fs.readFileBinary":
		case "fs.pread":
			return {
				kind: SYNC_BRIDGE_KIND_BINARY,
				value: decodeBinaryContent(result),
			};
		case "fs.realpath":
		case "fs.readlink":
			return {
				kind: SYNC_BRIDGE_KIND_TEXT,
				value: result.target ?? "",
			};
		case "fs.exists":
			return {
				kind: SYNC_BRIDGE_KIND_JSON,
				value: result.exists ?? false,
			};
		case "fs.stat":
		case "fs.lstat":
			return {
				kind: SYNC_BRIDGE_KIND_JSON,
				value: virtualStatToSyncJson(wireStatToVirtualStat(requireStat(result))),
			};
		case "fs.writeFile":
		case "fs.writeFileBinary":
		case "fs.pwrite":
		case "fs.createDir":
		case "fs.mkdir":
		case "fs.rmdir":
		case "fs.unlink":
		case "fs.rename":
		case "fs.symlink":
		case "fs.link":
		case "fs.chmod":
		case "fs.truncate":
			return { kind: SYNC_BRIDGE_KIND_NONE };
		default:
			throw new Error(
				`converged filesystem bridge has no response mapping for ${operation}`,
			);
	}
}

/**
 * Map a wire snake_case stat into the guest-facing camelCase `VirtualStat`.
 * The bigint u64 fields narrow to numbers here (lossy above
 * Number.MAX_SAFE_INTEGER, matching Node's default fs.stat).
 */
export function wireStatToVirtualStat(stat: LiveGuestFilesystemStat): VirtualStat {
	return {
		mode: stat.mode,
		size: Number(stat.size),
		sizeExact: stat.size,
		blocks: Number(stat.blocks),
		dev: Number(stat.dev),
		rdev: Number(stat.rdev),
		isDirectory: stat.is_directory,
		isSymbolicLink: stat.is_symbolic_link,
		atimeMs: stat.atime_ms,
		mtimeMs: stat.mtime_ms,
		ctimeMs: stat.ctime_ms,
		birthtimeMs: stat.birthtime_ms,
		ino: Number(stat.ino),
		inoExact: stat.ino,
		nlink: Number(stat.nlink),
		nlinkExact: stat.nlink,
		uid: stat.uid,
		gid: stat.gid,
	};
}

/**
 * Drop the optional `*Exact` bigint fields before a stat crosses the JSON
 * sync bridge: `JSON.stringify` throws on bigint, and the guest-side Node
 * shim only consumes the numeric fields anyway. In-process consumers (e.g.
 * the WASI filestat writer) keep the exact fields.
 */
export function virtualStatToSyncJson(stat: VirtualStat): VirtualStat {
	const {
		sizeExact: _sizeExact,
		inoExact: _inoExact,
		nlinkExact: _nlinkExact,
		...jsonStat
	} = stat;
	return jsonStat;
}

/** Build a `VirtualDirEntry` from a directory child name and its lstat. */
export function wireStatToDirEntry(
	name: string,
	stat: LiveGuestFilesystemStat,
): VirtualDirEntry {
	return {
		name,
		isDirectory: stat.is_directory,
		isSymbolicLink: stat.is_symbolic_link,
	};
}

function requireStat(result: GuestFilesystemResult): LiveGuestFilesystemStat {
	if (!result.stat) {
		throw new Error(
			`guest_filesystem_result for ${result.operation} is missing stat`,
		);
	}
	return result.stat;
}

function decodeTextContent(result: GuestFilesystemResult): string {
	const content = result.content ?? "";
	if (result.encoding === "base64") {
		return new TextDecoder().decode(decodeBase64(content));
	}
	return content;
}

function decodeBinaryContent(result: GuestFilesystemResult): Uint8Array {
	const content = result.content ?? "";
	if (result.encoding === "base64") {
		return decodeBase64(content);
	}
	return new TextEncoder().encode(content);
}

function toUint8Array(value: unknown): Uint8Array {
	if (value instanceof Uint8Array) {
		return value;
	}
	if (value instanceof ArrayBuffer) {
		return new Uint8Array(value);
	}
	if (ArrayBuffer.isView(value)) {
		return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
	}
	if (Array.isArray(value)) {
		return Uint8Array.from(value as number[]);
	}
	throw new Error("converged filesystem bridge expected binary content");
}
