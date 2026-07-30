import { describe, expect, it } from "vitest";
import { convergedPermissionsPolicy } from "../../src/converged-permissions.js";

describe("converged permissions policy", () => {
	it("defaults to allow-all", () => {
		expect(convergedPermissionsPolicy()).toMatchObject({
			fs: "allow",
			network: "allow",
			childProcess: "allow",
		});
	});

	it("expresses deny-fs-read as a read/readdir deny rule", () => {
		const policy = convergedPermissionsPolicy({ denyFsRead: true });
		expect(policy.fs).toEqual({
			default: "allow",
			rules: [{ mode: "deny", operations: ["read", "readdir"], paths: ["**"] }],
		});
	});

	it("expresses childProcess and network denials", () => {
		expect(convergedPermissionsPolicy({ denyChildProcess: true }).childProcess).toBe(
			"deny",
		);
		expect(convergedPermissionsPolicy({ denyNetwork: true }).network).toBe("deny");
	});

	it("expresses a denied network port as a tcp pattern rule", () => {
		const policy = convergedPermissionsPolicy({ denyNetworkPort: 8080 });
		expect(policy.network).toMatchObject({
			default: "allow",
			rules: [{ mode: "deny", patterns: ["tcp://*:8080"] }],
		});
	});
});
