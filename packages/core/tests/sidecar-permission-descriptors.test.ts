import { describe, expect, test } from "vitest";
import type { Permissions } from "../src/runtime-compat.js";
import { serializePermissionsForSidecar } from "../src/sidecar/permissions.js";

describe("serializePermissionsForSidecar", () => {
	test("uses deny-all policy when permissions are omitted", () => {
		expect(serializePermissionsForSidecar()).toEqual({
			fs: "deny",
			network: "deny",
			childProcess: "deny",
			process: "deny",
			env: "deny",
			tool: "deny",
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
			process: {
				default: "deny",
				rules: [
					{
						mode: "allow",
						operations: ["inspect"],
						patterns: ["**"],
					},
				],
			},
			tool: {
				default: "deny",
				rules: [
					{
						mode: "allow",
						operations: ["invoke"],
						patterns: ["math:*"],
					},
				],
			},
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
			process: {
				default: "deny",
				rules: [
					{
						mode: "allow",
						operations: ["inspect"],
						patterns: ["**"],
					},
				],
			},
			env: undefined,
			tool: {
				default: "deny",
				rules: [
					{
						mode: "allow",
						operations: ["invoke"],
						patterns: ["math:*"],
					},
				],
			},
		});
	});

	test("preserves partial policies so unspecified domains can be denied in Rust", () => {
		const permissions: Permissions = {
			env: {
				rules: [
					{
						mode: "allow",
						operations: ["*"],
						patterns: ["OPENAI_*", "PATH"],
					},
				],
			},
		};

		expect(serializePermissionsForSidecar(permissions)).toEqual({
			fs: undefined,
			network: undefined,
			childProcess: undefined,
			process: undefined,
			env: {
				rules: [
					{
						mode: "allow",
						operations: ["*"],
						patterns: ["OPENAI_*", "PATH"],
					},
				],
			},
			tool: undefined,
		});
	});

	test("expands omitted rule operations and resources to explicit wildcards", () => {
		const permissions: Permissions = {
			fs: {
				default: "deny",
				rules: [
					{
						mode: "allow",
						operations: ["read"],
					},
				],
			},
			network: {
				default: "deny",
				rules: [
					{
						mode: "allow",
						patterns: ["tcp://localhost:443"],
					},
				],
			},
		};

		expect(serializePermissionsForSidecar(permissions)).toEqual({
			fs: {
				default: "deny",
				rules: [
					{
						mode: "allow",
						operations: ["read"],
						paths: ["**"],
					},
				],
			},
			network: {
				default: "deny",
				rules: [
					{
						mode: "allow",
						operations: ["*"],
						patterns: ["tcp://localhost:443"],
					},
				],
			},
			childProcess: undefined,
			process: undefined,
			env: undefined,
			tool: undefined,
		});
	});
});
