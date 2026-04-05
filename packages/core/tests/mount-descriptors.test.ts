import { describe, expect, test } from "vitest";
import { createHostDirBackend, createInMemoryFileSystem } from "../src/index.js";
import { serializeMountConfigForSidecar } from "../src/sidecar/mount-descriptors.js";

describe("sidecar mount descriptors", () => {
	test("serializes declarative native host-dir mount configs", () => {
		expect(
			serializeMountConfigForSidecar({
				path: "/workspace",
				readOnly: true,
				plugin: createHostDirBackend({
					hostPath: "/tmp/project",
					readOnly: false,
				}),
			}),
		).toEqual({
			guestPath: "/workspace",
			readOnly: true,
			plugin: {
				id: "host_dir",
				config: {
					hostPath: "/tmp/project",
					readOnly: false,
				},
			},
		});
	});

	test("host-dir helper defaults config.readOnly to true", () => {
		expect(createHostDirBackend({ hostPath: "/tmp/project" })).toEqual({
			id: "host_dir",
			config: {
				hostPath: "/tmp/project",
				readOnly: true,
			},
		});
	});

	test("maps caller-supplied filesystems to the js_bridge fallback", () => {
		expect(
			serializeMountConfigForSidecar({
				path: "/custom",
				driver: createInMemoryFileSystem(),
				readOnly: false,
			}),
		).toEqual({
			guestPath: "/custom",
			readOnly: false,
			plugin: {
				id: "js_bridge",
				config: {},
			},
		});
	});
});
