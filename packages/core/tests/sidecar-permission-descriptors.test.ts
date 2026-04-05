import { describe, expect, test } from "vitest";
import type { Permissions } from "../src/runtime-compat.js";
import { serializePermissionsForSidecar } from "../src/sidecar/permission-descriptors.js";

describe("serializePermissionsForSidecar", () => {
	test("uses allow-all descriptors when permissions are omitted", () => {
		expect(serializePermissionsForSidecar()).toEqual([
			{ capability: "fs", mode: "allow" },
			{ capability: "network", mode: "allow" },
			{ capability: "child_process", mode: "allow" },
			{ capability: "env", mode: "allow" },
		]);
	});

	test("serializes per-operation fs restrictions and preserves env deny-by-default on partial policies", () => {
		const permissions: Permissions = {
			fs: ({ operation }) => operation === "read",
			network: () => false,
			childProcess: () => false,
		};

		expect(serializePermissionsForSidecar(permissions)).toEqual([
			{ capability: "fs.read", mode: "allow" },
			{ capability: "fs.write", mode: "deny" },
			{ capability: "fs.create_dir", mode: "deny" },
			{ capability: "fs.readdir", mode: "deny" },
			{ capability: "fs.stat", mode: "deny" },
			{ capability: "fs.rm", mode: "deny" },
			{ capability: "fs.rename", mode: "deny" },
			{ capability: "fs.symlink", mode: "deny" },
			{ capability: "fs.readlink", mode: "deny" },
			{ capability: "fs.chmod", mode: "deny" },
			{ capability: "fs.truncate", mode: "deny" },
			{ capability: "fs.mount_sensitive", mode: "deny" },
			{ capability: "network", mode: "deny" },
			{ capability: "child_process", mode: "deny" },
			{ capability: "env", mode: "deny" },
		]);
	});

	test("rejects resource-dependent permission callbacks that the native sidecar cannot serialize", () => {
		const permissions: Permissions = {
			fs: ({ path }) => path.startsWith("/workspace"),
		};

		expect(() => serializePermissionsForSidecar(permissions)).toThrow(
			/varies by resource/,
		);
	});
});
