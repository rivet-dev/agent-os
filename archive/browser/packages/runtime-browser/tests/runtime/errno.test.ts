import os from "node:os";
import { describe, expect, it } from "vitest";
import { posixErrno } from "../../src/errno.js";

describe("posixErrno", () => {
	it("matches Node's negated errno for every mapped code", () => {
		const nodeErrno = os.constants.errno as Record<string, number>;
		// Codes the old inline table got wrong (errno -1) plus the original few.
		const codes = [
			"ENOENT",
			"EACCES",
			"EEXIST",
			"ENOTDIR",
			"EISDIR",
			"EROFS",
			"ENOTEMPTY",
			"EBADF",
			"EXDEV",
			"EMFILE",
			"EPERM",
			"EINVAL",
			"ENOSYS",
			"ELOOP",
			"ENAMETOOLONG",
			"ECONNREFUSED",
			"ETIMEDOUT",
		];
		for (const code of codes) {
			expect(nodeErrno[code], `Node exposes ${code}`).toBeGreaterThan(0);
			expect(posixErrno(code), code).toBe(-nodeErrno[code]);
		}
	});

	it("returns undefined for unknown or missing codes", () => {
		expect(posixErrno("ENOTAREALCODE")).toBeUndefined();
		expect(posixErrno(undefined)).toBeUndefined();
	});
});
