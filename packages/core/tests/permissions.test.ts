import { describe, expect, it } from "vitest";
import * as protocol from "../src/generated-protocol.js";
import {
	toGeneratedFilesystemPermissionScope,
	toGeneratedPatternPermissionScope,
	toGeneratedPermissionsPolicy,
} from "../src/permissions.js";

describe("permissions", () => {
	it("returns null when no policy is supplied", () => {
		expect(toGeneratedPermissionsPolicy(undefined)).toBe(null);
	});

	it("maps direct permission modes", () => {
		expect(toGeneratedPermissionsPolicy({ fs: "deny" })).toMatchObject({
			fs: {
				tag: "PermissionMode",
				val: protocol.PermissionMode.Deny,
			},
		});
	});

	it("maps filesystem rule sets", () => {
		expect(
			toGeneratedFilesystemPermissionScope({
				default: "ask",
				rules: [
					{ mode: "allow", operations: ["read"], paths: ["/tmp"] },
				],
			}),
		).toEqual({
			tag: "FsPermissionRuleSet",
			val: {
				default: protocol.PermissionMode.Ask,
				rules: [
					{
						mode: protocol.PermissionMode.Allow,
						operations: ["read"],
						paths: ["/tmp"],
					},
				],
			},
		});
	});

	it("maps pattern rule sets", () => {
		expect(
			toGeneratedPatternPermissionScope({
				rules: [{ mode: "deny", patterns: ["*.local"] }],
			}),
		).toEqual({
			tag: "PatternPermissionRuleSet",
			val: {
				default: null,
				rules: [
					{
						mode: protocol.PermissionMode.Deny,
						operations: [],
						patterns: ["*.local"],
					},
				],
			},
		});
	});

	it("maps child_process to generated childProcess", () => {
		expect(
			toGeneratedPermissionsPolicy({
				child_process: {
					rules: [{ mode: "allow", operations: ["spawn"] }],
				},
			})?.childProcess,
		).toEqual({
			tag: "PatternPermissionRuleSet",
			val: {
				default: null,
				rules: [
					{
						mode: protocol.PermissionMode.Allow,
						operations: ["spawn"],
						patterns: [],
					},
				],
			},
		});
	});
});
