import { describe, expect, test } from "vitest";
import * as protocol from "../src/generated-protocol.js";
import {
	decodeGuestFilesystemContent,
	encodeGuestFilesystemContent,
	fromGeneratedRootFilesystemEntry,
	toGeneratedRootFilesystemDescriptor,
	toGeneratedRootFilesystemEntry,
} from "../src/filesystem.js";

describe("guest filesystem content helpers", () => {
	test("leaves string content as utf8 text", () => {
		expect(encodeGuestFilesystemContent("hello")).toEqual({
			content: "hello",
		});
	});

	test("encodes and decodes binary content as base64", () => {
		const encoded = encodeGuestFilesystemContent(new Uint8Array([0, 1, 2, 255]));

		expect(encoded).toEqual({
			content: "AAEC/w==",
			encoding: "base64",
		});
		expect(
			Array.from(
				decodeGuestFilesystemContent({
					path: "/tmp/blob",
					content: encoded.content,
					encoding: encoded.encoding,
				}),
			),
		).toEqual([0, 1, 2, 255]);
	});

	test("throws when read responses omit content", () => {
		expect(() => decodeGuestFilesystemContent({ path: "/tmp/missing" })).toThrow(
			"sidecar returned no file content for /tmp/missing",
		);
	});

	test("maps live root filesystem entries to generated protocol entries", () => {
		expect(
			toGeneratedRootFilesystemEntry({
				path: "/bin/tool",
				kind: "file",
				mode: 0o755,
				content: "echo ok",
				encoding: "utf8",
				executable: true,
			}),
		).toEqual({
			path: "/bin/tool",
			kind: protocol.RootFilesystemEntryKind.File,
			mode: 0o755,
			uid: null,
			gid: null,
			content: "echo ok",
			encoding: protocol.RootFilesystemEntryEncoding.UtF8,
			target: null,
			executable: true,
		});
	});

	test("maps generated root filesystem entries to live entries", () => {
		expect(
			fromGeneratedRootFilesystemEntry({
				path: "/link",
				kind: protocol.RootFilesystemEntryKind.Symlink,
				mode: null,
				uid: null,
				gid: null,
				content: null,
				encoding: null,
				target: "/target",
				executable: false,
			}),
		).toEqual({
			path: "/link",
			kind: "symlink",
			target: "/target",
			executable: false,
		});
	});

	test("maps live root filesystem descriptors to generated descriptors", () => {
		expect(
			toGeneratedRootFilesystemDescriptor({
				mode: "read_only",
				disable_default_base_layer: true,
				lowers: [
					{
						kind: "snapshot",
						entries: [{ path: "/etc/app", kind: "directory" }],
					},
					{ kind: "bundled_base_filesystem" },
				],
				bootstrap_entries: [
					{ path: "/etc/app/config.json", kind: "file", content: "{}" },
				],
			}),
		).toMatchObject({
			mode: protocol.RootFilesystemMode.ReadOnly,
			disableDefaultBaseLayer: true,
			lowers: [
				{ tag: "SnapshotRootFilesystemLower" },
				{ tag: "BundledBaseFilesystemLower", val: null },
			],
			bootstrapEntries: [
				{
					path: "/etc/app/config.json",
					kind: protocol.RootFilesystemEntryKind.File,
					content: "{}",
				},
			],
		});
	});
});
