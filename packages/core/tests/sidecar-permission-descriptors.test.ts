import { describe, expect, test } from "vitest";
import type { Permissions } from "../src/runtime-compat.js";
import { serializePermissionsForSidecar } from "../src/sidecar/permissions.js";

describe("serializePermissionsForSidecar", () => {
	test("uses allow-all policy when permissions are omitted", () => {
		expect(serializePermissionsForSidecar()).toEqual({
			fs: "allow",
			network: "allow",
			childProcess: "allow",
			env: "allow",
		});
	});

	test("passes structured declarative policies through unchanged", () => {
		const permissions: Permissions = {
			fs: {
				default: "deny",
				rules: [
					{
						mode: "allow",
						operations: ["read"],
						paths: ["/workspace/**"],
					},
				],
			},
			network: {
				default: "deny",
				rules: [
					{
						mode: "allow",
						operations: ["dns"],
						patterns: ["dns://*.example.test"],
					},
				],
			},
			childProcess: "deny",
		};

		expect(serializePermissionsForSidecar(permissions)).toEqual({
			fs: {
				default: "deny",
				rules: [
					{
						mode: "allow",
						operations: ["read"],
						paths: ["/workspace/**"],
					},
				],
			},
			network: {
				default: "deny",
				rules: [
					{
						mode: "allow",
						operations: ["dns"],
						patterns: ["dns://*.example.test"],
					},
				],
			},
			childProcess: "deny",
			env: undefined,
		});
	});

	test("preserves partial policies so unspecified domains can be denied in Rust", () => {
		const permissions: Permissions = {
			env: {
				rules: [
					{
						mode: "allow",
						patterns: ["OPENAI_*", "PATH"],
					},
				],
			},
		};

		expect(serializePermissionsForSidecar(permissions)).toEqual({
			fs: undefined,
			network: undefined,
			childProcess: undefined,
			env: {
				rules: [
					{
						mode: "allow",
						patterns: ["OPENAI_*", "PATH"],
					},
				],
			},
		});
	});
});
