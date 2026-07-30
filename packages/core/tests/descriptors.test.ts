import { describe, expect, it } from "vitest";
import {
	toGeneratedMountDescriptor,
	toGeneratedProjectedModuleDescriptor,
	toGeneratedSidecarPlacement,
	toGeneratedSoftwareDescriptor,
} from "../src/descriptors.js";

describe("descriptors", () => {
	it("maps shared and explicit sidecar placements", () => {
		expect(toGeneratedSidecarPlacement({ kind: "shared" })).toEqual({
			tag: "SidecarPlacementShared",
			val: { pool: null },
		});
		expect(
			toGeneratedSidecarPlacement({ kind: "shared", pool: "workers" }),
		).toEqual({
			tag: "SidecarPlacementShared",
			val: { pool: "workers" },
		});
		expect(
			toGeneratedSidecarPlacement({
				kind: "explicit",
				sidecar_id: "sidecar-1",
			}),
		).toEqual({
			tag: "SidecarPlacementExplicit",
			val: { sidecarId: "sidecar-1" },
		});
	});

	it("maps mount descriptors and serializes plugin config as JSON", () => {
		expect(
			toGeneratedMountDescriptor({
				guest_path: "/workspace",
				read_only: true,
				plugin: {
					id: "host",
					config: { source: "/tmp/project", depth: 2 },
				},
			}),
		).toEqual({
			guestPath: "/workspace",
			guestSource: "host",
			guestFstype: "host",
			readOnly: true,
			plugin: {
				id: "host",
				config: '{"source":"/tmp/project","depth":2}',
			},
		});
	});

	it("maps software and projected module descriptors", () => {
		expect(
			toGeneratedSoftwareDescriptor({
				package_name: "node",
				root: "/software/node",
			}),
		).toEqual({
			packageName: "node",
			root: "/software/node",
		});
		expect(
			toGeneratedProjectedModuleDescriptor({
				package_name: "@acme/tool",
				entrypoint: "dist/index.js",
			}),
		).toEqual({
			packageName: "@acme/tool",
			entrypoint: "dist/index.js",
		});
	});
});
