import { existsSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import {
	createInMemoryFileSystem,
	createKernel,
	createWasmVmRuntime,
	describeIf,
	type Kernel,
} from "@rivet-dev/agentos-test-harness";
import { afterEach, beforeEach, expect, it } from "vitest";

const XFS_COMMAND_DIR = fileURLToPath(new URL("../bin", import.meta.url));
const hasXfsIo = existsSync(join(XFS_COMMAND_DIR, "xfs_io"));

describeIf(hasXfsIo, "xfs_io command", { timeout: 30_000 }, () => {
	let filesystem: ReturnType<typeof createInMemoryFileSystem>;
	let kernel: Kernel | undefined;

	beforeEach(async () => {
		filesystem = createInMemoryFileSystem();
		await filesystem.mkdir("/workspace", { recursive: true });
		await filesystem.chown("/workspace", 1000, 1000);
		kernel = createKernel({ filesystem });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [XFS_COMMAND_DIR] }));
	}, 60_000);

	afterEach(async () => {
		await kernel?.dispose();
		kernel = undefined;
	}, 60_000);

	async function run(args: string[]) {
		if (!kernel) throw new Error("kernel not mounted");
		let stdout = "";
		let stderr = "";
		const process = kernel.spawn("xfs_io", args, {
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});
		const exitCode = await process.wait();
		await new Promise<void>((resolve) => setTimeout(resolve, 0));
		return { exitCode, stdout, stderr };
	}

	it("writes, truncates, and reports metadata for a kernel-backed file", async () => {
		const path = "/workspace/data.bin";
		const result = await run([
			"-f",
			"-c",
			"pwrite -q -S 0x41 0 8",
			"-c",
			"truncate 12",
			"-c",
			"stat",
			path,
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stderr).toBe("");
		expect(result.stdout).toContain(`fd.path = "${path}"`);
		expect(result.stdout).toContain("stat.size = 12");

		const bytes = await filesystem.readFile(path);
		expect(Array.from(bytes)).toEqual([
			0x41,
			0x41,
			0x41,
			0x41,
			0x41,
			0x41,
			0x41,
			0x41,
			0,
			0,
			0,
			0,
		]);
	});

	it("punches a complete extent and exposes the resulting hole", async () => {
		const path = "/workspace/sparse.bin";
		const result = await run([
			"-f",
			"-c",
			"pwrite -q -S 0x7a 0 1536",
			"-c",
			"fpunch 512 512",
			"-c",
			"fiemap -v",
			path,
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stderr).toBe("");
		expect(result.stdout).toContain("hole");

		const bytes = await filesystem.readFile(path);
		expect(bytes).toHaveLength(1536);
		expect(bytes.slice(0, 512).every((byte) => byte === 0x7a)).toBe(true);
		expect(bytes.slice(512, 1024).every((byte) => byte === 0)).toBe(true);
		expect(bytes.slice(1024).every((byte) => byte === 0x7a)).toBe(true);
	});

	it("links the open description and persists nanosecond timestamps", async () => {
		const path = "/workspace/original.bin";
		const linkedPath = "/workspace/linked.bin";
		const result = await run([
			"-f",
			"-c",
			"pwrite -q -S 0x2a 0 4",
			"-c",
			"utimes 123 456000000 789 123000000",
			"-c",
			`flink ${linkedPath}`,
			path,
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stdout).toBe("");
		expect(result.stderr).toBe("");

		const originalStat = await filesystem.stat(path);
		const linkedStat = await filesystem.stat(linkedPath);
		expect(linkedStat.ino).toBe(originalStat.ino);
		expect(originalStat.nlink).toBe(2);
		expect(linkedStat.nlink).toBe(2);
		expect(originalStat.atimeMs).toBe(123_456);
		expect(originalStat.mtimeMs).toBe(789_123);

		const original = await filesystem.readFile(path);
		const linked = await filesystem.readFile(linkedPath);
		expect(Array.from(original)).toEqual([0x2a, 0x2a, 0x2a, 0x2a]);
		expect(Array.from(linked)).toEqual(Array.from(original));
	});
});
