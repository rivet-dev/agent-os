import { describe, expect, it } from "vitest";
import type { LiveGuestFilesystemStat } from "@rivet-dev/agentos-runtime-core";
import {
	convergedFilesystemRequestPayload,
	convergedFilesystemSyncResponse,
	type GuestFilesystemResult,
	isSingleCallFilesystemOperation,
	virtualStatToSyncJson,
	wireStatToDirEntry,
	wireStatToVirtualStat,
} from "../../src/converged-fs-bridge.js";
import {
	SYNC_BRIDGE_KIND_BINARY,
	SYNC_BRIDGE_KIND_JSON,
	SYNC_BRIDGE_KIND_NONE,
	SYNC_BRIDGE_KIND_TEXT,
} from "../../src/sync-bridge.js";

const SAMPLE_STAT: LiveGuestFilesystemStat = {
	mode: 0o100644,
	size: 12n,
	blocks: 1n,
	dev: 2n,
	rdev: 0n,
	is_directory: false,
	is_symbolic_link: false,
	atime_ms: 10,
	mtime_ms: 20,
	ctime_ms: 30,
	birthtime_ms: 40,
	ino: 7n,
	nlink: 1n,
	uid: 1000,
	gid: 1000,
};

function result(
	partial: Partial<GuestFilesystemResult> & {
		operation: GuestFilesystemResult["operation"];
	},
): GuestFilesystemResult {
	return { path: "/tmp/x", ...partial } as GuestFilesystemResult;
}

describe("converged filesystem bridge translation", () => {
	it("maps read/write requests to wire guest_filesystem_call payloads", () => {
		expect(
			convergedFilesystemRequestPayload("fs.readFile", ["/tmp/a.txt"]),
		).toEqual({
			type: "guest_filesystem_call",
			operation: "read_file",
			path: "/tmp/a.txt",
		});

		expect(
			convergedFilesystemRequestPayload("fs.writeFile", ["/tmp/a.txt", "hi"]),
		).toEqual({
			type: "guest_filesystem_call",
			operation: "write_file",
			path: "/tmp/a.txt",
			content: "hi",
			encoding: "utf8",
		});

		const binary = convergedFilesystemRequestPayload("fs.writeFileBinary", [
			"/tmp/blob",
			new Uint8Array([0, 1, 2, 253]),
		]);
		expect(binary).toMatchObject({
			operation: "write_file",
			path: "/tmp/blob",
			encoding: "base64",
		});
		expect(binary.content).toBe("AAEC/Q==");

		// Positional write: offset + base64 content, serviced by the kernel's
		// atomic pwrite rather than a client-side read-modify-write.
		const pwrite = convergedFilesystemRequestPayload("fs.pwrite", [
			"/tmp/blob",
			4,
			new Uint8Array([9, 8, 7]),
		]);
		expect(pwrite).toMatchObject({
			type: "guest_filesystem_call",
			operation: "pwrite",
			path: "/tmp/blob",
			offset: 4,
			encoding: "base64",
		});
		expect(pwrite.content).toBe("CQgH");
		expect(isSingleCallFilesystemOperation("fs.pwrite")).toBe(true);
		expect(
			convergedFilesystemSyncResponse(
				"fs.pwrite",
				result({ operation: "pwrite" }),
			),
		).toEqual({ kind: SYNC_BRIDGE_KIND_NONE });
	});

	it("maps metadata and link requests, including symlink arg order", () => {
		expect(convergedFilesystemRequestPayload("fs.mkdir", ["/d"])).toEqual({
			type: "guest_filesystem_call",
			operation: "mkdir",
			path: "/d",
			recursive: true,
		});
		expect(
			convergedFilesystemRequestPayload("fs.rename", ["/a", "/b"]),
		).toEqual({
			type: "guest_filesystem_call",
			operation: "rename",
			path: "/a",
			destination_path: "/b",
		});
		// Legacy symlink(target, linkPath) -> wire { path: link, target }.
		expect(
			convergedFilesystemRequestPayload("fs.symlink", ["/target", "/link"]),
		).toEqual({
			type: "guest_filesystem_call",
			operation: "symlink",
			path: "/link",
			target: "/target",
		});
		expect(
			convergedFilesystemRequestPayload("fs.chmod", ["/a", 0o755]),
		).toEqual({
			type: "guest_filesystem_call",
			operation: "chmod",
			path: "/a",
			mode: 0o755,
		});
	});

	it("throws for non-single-call operations (readDir)", () => {
		expect(isSingleCallFilesystemOperation("fs.readDir")).toBe(false);
		expect(() =>
			convergedFilesystemRequestPayload("fs.readDir", ["/d"]),
		).toThrow(/no single-call mapping/);
	});

	it("decodes read results to text and binary by original operation", () => {
		const textResponse = convergedFilesystemSyncResponse(
			"fs.readFile",
			result({ operation: "read_file", content: "hello", encoding: "utf8" }),
		);
		expect(textResponse).toEqual({
			kind: SYNC_BRIDGE_KIND_TEXT,
			value: "hello",
		});

		const binaryResponse = convergedFilesystemSyncResponse(
			"fs.readFileBinary",
			result({ operation: "read_file", content: "AAEC/Q==", encoding: "base64" }),
		);
		expect(binaryResponse.kind).toBe(SYNC_BRIDGE_KIND_BINARY);
		expect(
			binaryResponse.kind === SYNC_BRIDGE_KIND_BINARY
				? Array.from(binaryResponse.value)
				: null,
		).toEqual([0, 1, 2, 253]);
	});

	it("maps stat results to the guest camelCase VirtualStat shape", () => {
		const response = convergedFilesystemSyncResponse(
			"fs.stat",
			result({ operation: "stat", stat: SAMPLE_STAT }),
		);
		expect(response).toEqual({
			kind: SYNC_BRIDGE_KIND_JSON,
			value: virtualStatToSyncJson(wireStatToVirtualStat(SAMPLE_STAT)),
		});
		expect(wireStatToVirtualStat(SAMPLE_STAT)).toMatchObject({
			isDirectory: false,
			isSymbolicLink: false,
			mtimeMs: 20,
			birthtimeMs: 40,
		});
	});

	it("keeps stat sync responses JSON-serializable despite bigint exact fields", () => {
		const unsafe = BigInt(Number.MAX_SAFE_INTEGER) + 2n;
		const response = convergedFilesystemSyncResponse(
			"fs.stat",
			result({
				operation: "stat",
				stat: { ...SAMPLE_STAT, size: unsafe, ino: unsafe, nlink: unsafe },
			}),
		);
		if (response.kind !== SYNC_BRIDGE_KIND_JSON) {
			throw new Error("expected JSON sync response");
		}
		// JSON.stringify throws on bigint; the sync bridge encodes with it.
		const encoded = JSON.stringify(response.value);
		const decoded = JSON.parse(encoded);
		expect(decoded.ino).toBe(Number(unsafe));
		expect(decoded.size).toBe(Number(unsafe));
		expect(decoded).not.toHaveProperty("inoExact");
		expect(decoded).not.toHaveProperty("sizeExact");
		expect(decoded).not.toHaveProperty("nlinkExact");
	});

	it("returns NONE for mutations and JSON booleans for exists", () => {
		expect(
			convergedFilesystemSyncResponse(
				"fs.writeFile",
				result({ operation: "write_file" }),
			),
		).toEqual({ kind: SYNC_BRIDGE_KIND_NONE });
		expect(
			convergedFilesystemSyncResponse(
				"fs.exists",
				result({ operation: "exists", exists: true }),
			),
		).toEqual({ kind: SYNC_BRIDGE_KIND_JSON, value: true });
	});

	it("builds typed dir entries from a child name + lstat", () => {
		expect(
			wireStatToDirEntry("child", { ...SAMPLE_STAT, is_directory: true }),
		).toEqual({ name: "child", isDirectory: true, isSymbolicLink: false });
	});
});
