import { describe, expect, it } from "vitest";
import type { ConvergedSidecarRequestTransport } from "../../src/converged-sync-bridge-handler.js";
import { ConvergedModuleServicer } from "../../src/converged-module-servicer.js";
import { KernelBackedFilesystem } from "../../src/kernel-backed-filesystem.js";
import {
	SYNC_BRIDGE_KIND_NONE,
	SYNC_BRIDGE_KIND_TEXT,
} from "../../src/sync-bridge.js";

// Model a tiny on-kernel filesystem: /app/index.js requires ./util, and a
// package "left-pad" under /app/node_modules with a package.json main.
function packageTransport(): ConvergedSidecarRequestTransport {
	const files = new Map<string, string>([
		["/app/index.js", "require('./util'); require('left-pad');"],
		["/app/util.js", "module.exports = 1;"],
		[
			"/app/node_modules/left-pad/package.json",
			JSON.stringify({ name: "left-pad", version: "1.0.0", main: "index.js" }),
		],
		["/app/node_modules/left-pad/index.js", "module.exports = () => {};"],
	]);
	const dirs = new Set<string>([
		"/app",
		"/app/node_modules",
		"/app/node_modules/left-pad",
	]);
	const exists = (p: string) => files.has(p) || dirs.has(p);
	return {
		sendRequest(payload) {
			if (payload.type !== "guest_filesystem_call") {
				return { type: "rejected", code: "x", message: payload.type };
			}
			const path = payload.path;
			const result = (extra: Record<string, unknown>) => ({
				type: "guest_filesystem_result" as const,
				operation: payload.operation,
				path,
				...extra,
			});
			switch (payload.operation) {
				case "exists":
					return result({ exists: exists(path) });
				case "read_file":
					if (!files.has(path)) {
						return { type: "rejected", code: "ENOENT", message: `no ${path}` };
					}
					return result({ content: files.get(path), encoding: "utf8" });
				case "realpath":
					return result({ target: path });
				case "stat":
				case "lstat": {
					if (!exists(path)) {
						return { type: "rejected", code: "ENOENT", message: `no ${path}` };
					}
					const isDir = dirs.has(path);
					return result({
						stat: {
							mode: isDir ? 0o040755 : 0o100644,
							size: 1,
							blocks: 1,
							dev: 1,
							rdev: 0,
							is_directory: isDir,
							is_symbolic_link: false,
							atime_ms: 0,
							mtime_ms: 0,
							ctime_ms: 0,
							birthtime_ms: 0,
							ino: 1,
							nlink: 1,
							uid: 0,
							gid: 0,
						},
					});
				}
				default:
					return result({});
			}
		},
	};
}

describe("converged module servicer", () => {
	const servicer = new ConvergedModuleServicer(
		new KernelBackedFilesystem(packageTransport()),
	);

	it("declares the module operation family", () => {
		expect(servicer.handles("module.resolve")).toBe(true);
		expect(servicer.handles("module.batchResolve")).toBe(true);
		expect(servicer.handles("fs.readFile")).toBe(false);
	});

	it("resolves a relative import against the kernel filesystem", async () => {
		const response = await servicer.handle("module.resolve", [
			"./util",
			"/app/index.js",
			"require",
		]);
		expect(response).toEqual({
			kind: SYNC_BRIDGE_KIND_TEXT,
			value: "/app/util.js",
		});
	});

	it("resolves a bare package via node_modules + package.json main", async () => {
		const response = await servicer.handle("module.resolve", [
			"left-pad",
			"/app/index.js",
			"require",
		]);
		expect(response).toEqual({
			kind: SYNC_BRIDGE_KIND_TEXT,
			value: "/app/node_modules/left-pad/index.js",
		});
	});

	it("returns NONE for an unresolvable specifier", async () => {
		const response = await servicer.handle("module.resolve", [
			"does-not-exist",
			"/app/index.js",
			"require",
		]);
		expect(response).toEqual({ kind: SYNC_BRIDGE_KIND_NONE });
	});

	it("loads file source through the kernel", async () => {
		const response = await servicer.handle("module.loadFile", ["/app/util.js"]);
		expect(response).toEqual({
			kind: SYNC_BRIDGE_KIND_TEXT,
			value: "module.exports = 1;",
		});
	});
});
