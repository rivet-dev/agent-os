import { describe, expect, test } from "vitest";
import type { FilesystemEntry } from "../../src/filesystem-snapshot.js";
import { createSnapshotExport } from "../../src/index.js";
import { getBaseFilesystemEntries } from "../../src/base-filesystem.js";
import { serializeRootFilesystemForSidecar } from "../../src/sidecar/root-filesystem-descriptors.js";

function toExpectedSidecarEntry(entry: FilesystemEntry) {
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

describe("sidecar root filesystem descriptors", () => {
	test("serializes explicit lowers and bootstrap snapshots without changing the host config shape", () => {
		const configLower = createSnapshotExport([
			{
				path: "/workspace",
				type: "directory",
				mode: "0755",
				uid: 0,
				gid: 0,
			},
			{
				path: "/workspace/run.sh",
				type: "file",
				mode: "0755",
				uid: 0,
				gid: 0,
				content: "echo hi\n",
				encoding: "utf8",
			},
			{
				path: "/workspace/payload.bin",
				type: "file",
				mode: "0644",
				uid: 1000,
				gid: 1000,
				content: "AAEC",
				encoding: "base64",
			},
			{
				path: "/workspace/current",
				type: "symlink",
				mode: "0777",
				uid: 0,
				gid: 0,
				target: "/workspace/run.sh",
			},
		]);
		const bootstrapLower = createSnapshotExport([
			{
				path: "/bin/tool",
				type: "file",
				mode: "0755",
				uid: 0,
				gid: 0,
				content: "#!/bin/sh\nexit 0\n",
				encoding: "utf8",
			},
		]);

		expect(
			serializeRootFilesystemForSidecar(
				{
					mode: "read-only",
					disableDefaultBaseLayer: true,
					lowers: [configLower],
				},
				bootstrapLower,
			),
		).toEqual({
			mode: "read_only",
			disableDefaultBaseLayer: true,
			lowers: [
				{
					kind: "snapshot",
					entries: configLower.source.filesystem.entries.map(toExpectedSidecarEntry),
				},
				{
					kind: "snapshot",
					entries: bootstrapLower.source.filesystem.entries.map(toExpectedSidecarEntry),
				},
			],
			bootstrapEntries: [],
		});
	});

	test("inlines the bundled base lower when callers place it explicitly", () => {
		const descriptor = serializeRootFilesystemForSidecar({
			disableDefaultBaseLayer: true,
			lowers: [{ kind: "bundled-base-filesystem" }],
		});

		expect(descriptor.mode).toBe("ephemeral");
		expect(descriptor.disableDefaultBaseLayer).toBe(true);
		expect(descriptor.bootstrapEntries).toEqual([]);
		expect(descriptor.lowers).toHaveLength(1);
		expect(descriptor.lowers[0]).toEqual({
			kind: "snapshot",
			entries: getBaseFilesystemEntries().map(toExpectedSidecarEntry),
		});
	});
});
