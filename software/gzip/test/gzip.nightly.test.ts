// Nightly: requires a non-core registry command.
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
	NodeFileSystem,
	createKernel,
	createWasmVmRuntime,
	describeIf, wasmBackendTestTimeout,
} from "@rivet-dev/agentos-test-harness";
import type { Kernel } from "@rivet-dev/agentos-test-harness";
import { afterEach, expect, it } from "vitest";

const GZIP_COMMAND_DIR = fileURLToPath(new URL("../bin", import.meta.url));
const hasGzipPackageBinary = existsSync(join(GZIP_COMMAND_DIR, "gzip"));

let tempRoot: string | undefined;

async function writeFixture(path: string, contents: string): Promise<void> {
	if (!tempRoot) throw new Error("fixture root not initialized");
	const hostPath = join(tempRoot, path.replace(/^\/+/, ""));
	await mkdir(dirname(hostPath), { recursive: true });
	await writeFile(hostPath, contents);
}

async function createTestVFS(): Promise<NodeFileSystem> {
	tempRoot = await mkdtemp(join(tmpdir(), "agentos-gzip-"));
	await writeFixture("/project/report.txt", "alpha\nbeta\ngamma\n");
	await writeFixture("/project/remove.txt", "temporary payload\n");
	return new NodeFileSystem({ root: tempRoot });
}

const textDecoder = new TextDecoder();

describeIf(hasGzipPackageBinary, "gzip command", { timeout: wasmBackendTestTimeout(10_000, 30_000) }, () => {
	let kernel: Kernel | undefined;

	afterEach(async () => {
		await kernel?.dispose();
		kernel = undefined;
		if (tempRoot) {
			await rm(tempRoot, { recursive: true, force: true });
			tempRoot = undefined;
		}
	});

	async function mountFixture(): Promise<void> {
		const vfs = await createTestVFS();
		kernel = createKernel({ filesystem: vfs });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [GZIP_COMMAND_DIR] }));
	}

	async function runCommand(command: string, args: string[]) {
		if (!kernel) throw new Error("kernel not mounted");
		let stdout = "";
		let stderr = "";
		const proc = kernel.spawn(command, args, {
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});
		const exitCode = await proc.wait();
		await new Promise<void>((resolve) => setTimeout(resolve, 0));
		return { stdout, stderr, exitCode };
	}

	async function readGuestText(path: string): Promise<string> {
		if (!kernel) throw new Error("kernel not mounted");
		return textDecoder.decode(await kernel.readFile(path));
	}

	it("compresses files while keeping originals with -k", async () => {
		await mountFixture();

		const result = await runCommand("gzip", ["-k", "/project/report.txt"]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		await expect(readGuestText("/project/report.txt")).resolves.toBe(
			"alpha\nbeta\ngamma\n",
		);

		const decompressed = await runCommand("gunzip", [
			"-c",
			"/project/report.txt.gz",
		]);
		expect(decompressed.exitCode, decompressed.stderr || decompressed.stdout).toBe(0);
		expect(decompressed.stdout).toBe("alpha\nbeta\ngamma\n");
	});

	it("removes the source file unless -k is set", async () => {
		await mountFixture();

		const result = await runCommand("gzip", ["/project/remove.txt"]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		await expect(readGuestText("/project/remove.txt")).rejects.toThrow();
		await expect(readGuestText("/project/remove.txt.gz")).resolves.toBeTruthy();
	});

	it("decompresses files with gunzip -k", async () => {
		await mountFixture();
		const compressed = await runCommand("gzip", ["-k", "/project/report.txt"]);
		expect(compressed.exitCode, compressed.stderr || compressed.stdout).toBe(0);
		await writeFixture("/project/report.txt", "replacement\n");

		const result = await runCommand("gunzip", [
			"-fk",
			"/project/report.txt.gz",
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		await expect(readGuestText("/project/report.txt")).resolves.toBe(
			"alpha\nbeta\ngamma\n",
		);
		await expect(readGuestText("/project/report.txt.gz")).resolves.toBeTruthy();
	});

	it("streams decompressed content with zcat", async () => {
		await mountFixture();
		const compressed = await runCommand("gzip", ["-k", "/project/report.txt"]);
		expect(compressed.exitCode, compressed.stderr || compressed.stdout).toBe(0);

		const result = await runCommand("zcat", ["/project/report.txt.gz"]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stdout).toBe("alpha\nbeta\ngamma\n");
	});

	it("fails instead of overwriting compressed outputs without -f", async () => {
		await mountFixture();
		const first = await runCommand("gzip", ["-k", "/project/report.txt"]);
		expect(first.exitCode, first.stderr || first.stdout).toBe(0);

		const second = await runCommand("gzip", ["-k", "/project/report.txt"]);
		expect(second.exitCode).not.toBe(0);
		expect(second.stderr).toContain("already exists");
	});
});
