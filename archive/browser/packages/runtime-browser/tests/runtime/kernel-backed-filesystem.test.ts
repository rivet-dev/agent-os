import { describe, expect, it } from "vitest";
import type { LiveGuestFilesystemStat } from "@rivet-dev/agentos-runtime-core";
import type { ConvergedSidecarRequestTransport } from "../../src/converged-sync-bridge-handler.js";
import { KernelBackedFilesystem } from "../../src/kernel-backed-filesystem.js";
import { encodeBase64 } from "../../src/converged-base64.js";

function statFor(name: string): LiveGuestFilesystemStat {
	return {
		mode: 0o100644,
		size: 3,
		blocks: 1,
		dev: 1,
		rdev: 0,
		is_directory: name === "sub",
		is_symbolic_link: false,
		atime_ms: 1,
		mtime_ms: 2,
		ctime_ms: 3,
		birthtime_ms: 4,
		ino: 5,
		nlink: 1,
		uid: 0,
		gid: 0,
	};
}

// In-memory transport modelling the kernel for the fs operations exercised here.
function memoryTransport(): ConvergedSidecarRequestTransport {
	const files = new Map<string, string>([["/pkg/index.js", "module.exports={}"]]);
	return {
		sendRequest(payload) {
			if (payload.type !== "guest_filesystem_call") {
				return { type: "rejected", code: "x", message: payload.type };
			}
			switch (payload.operation) {
				case "read_file":
					return {
						type: "guest_filesystem_result",
						operation: "read_file",
						path: payload.path,
						content: files.get(payload.path) ?? "",
						encoding: "utf8",
					};
				case "write_file":
					files.set(payload.path, payload.content ?? "");
					return {
						type: "guest_filesystem_result",
						operation: "write_file",
						path: payload.path,
					};
				case "exists":
					return {
						type: "guest_filesystem_result",
						operation: "exists",
						path: payload.path,
						exists: files.has(payload.path),
					};
				case "read_dir":
					return {
						type: "guest_filesystem_result",
						operation: "read_dir",
						path: payload.path,
						entries: [
							{ name: "index.js", isDirectory: false, isSymbolicLink: false },
							{ name: "sub", isDirectory: true, isSymbolicLink: false },
						],
					};
				case "lstat":
				case "stat":
					return {
						type: "guest_filesystem_result",
						operation: payload.operation,
						path: payload.path,
						stat: statFor(payload.path.split("/").pop() ?? ""),
					};
				case "realpath":
					return {
						type: "guest_filesystem_result",
						operation: "realpath",
						path: payload.path,
						target: payload.path,
					};
				default:
					return {
						type: "guest_filesystem_result",
						operation: payload.operation,
						path: payload.path,
					};
			}
		},
	};
}

describe("kernel-backed filesystem", () => {
	const fs = new KernelBackedFilesystem(memoryTransport());

	it("reads text and binary through the wire", async () => {
		expect(await fs.readTextFile("/pkg/index.js")).toBe("module.exports={}");
		await fs.writeFile("/bin", encodeBase64.length ? new Uint8Array([1, 2, 3]) : "");
	});

	it("reports existence and realpath", async () => {
		expect(await fs.exists("/pkg/index.js")).toBe(true);
		expect(await fs.exists("/missing")).toBe(false);
		expect(await fs.realpath("/pkg/index.js")).toBe("/pkg/index.js");
	});

	it("returns typed dir entries from a single read_dir round-trip", async () => {
		expect(await fs.readDirWithTypes("/pkg")).toEqual([
			{ name: "index.js", isDirectory: false, isSymbolicLink: false },
			{ name: "sub", isDirectory: true, isSymbolicLink: false },
		]);
	});

	it("returns names from read_dir", async () => {
		expect(await fs.readDir("/pkg")).toEqual(["index.js", "sub"]);
	});

	it("maps wire stat to camelCase VirtualStat", async () => {
		const stat = await fs.stat("/pkg/index.js");
		expect(stat.isDirectory).toBe(false);
		expect(stat.mtimeMs).toBe(2);
	});

	it("throws ENOSYS for pwrite", async () => {
		await expect(fs.pwrite()).rejects.toThrow(/ENOSYS/);
	});
});
