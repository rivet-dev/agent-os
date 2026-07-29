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
	describeIf,
} from "@rivet-dev/agentos-test-harness";
import type { Kernel } from "@rivet-dev/agentos-test-harness";
import { afterEach, expect, it } from "vitest";

const TAR_COMMAND_DIR = fileURLToPath(new URL("../bin", import.meta.url));
const hasTarPackageBinary = existsSync(join(TAR_COMMAND_DIR, "tar"));

let tempRoot: string | undefined;

function hostPath(path: string): string {
	if (!tempRoot) throw new Error("fixture root not initialized");
	return join(tempRoot, path.replace(/^\/+/, ""));
}

async function writeFixture(path: string, contents: string): Promise<void> {
	const target = hostPath(path);
	await mkdir(dirname(target), { recursive: true });
	await writeFile(target, contents);
}

async function createTestVFS(): Promise<NodeFileSystem> {
	tempRoot = await mkdtemp(join(tmpdir(), "agentos-tar-"));
	await writeFixture("/project/src/docs/readme.txt", "hello from docs\n");
	await writeFixture("/project/src/data/values.csv", "name,score\nAda,7\nLinus,3\n");
	await mkdir(hostPath("/project/out"), { recursive: true });
	await mkdir(hostPath("/project/strip"), { recursive: true });
	await mkdir(hostPath("/project/gzip-out"), { recursive: true });
	return new NodeFileSystem({ root: tempRoot });
}

function lines(stdout: string): string[] {
	return stdout.split("\n").filter((line) => line.length > 0);
}

const textDecoder = new TextDecoder();

describeIf(hasTarPackageBinary, "tar command", { timeout: 10_000 }, () => {
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
		await kernel.mount(createWasmVmRuntime({ commandDirs: [TAR_COMMAND_DIR] }));
	}

	async function runTar(args: string[]) {
		if (!kernel) throw new Error("kernel not mounted");
		let stdout = "";
		let stderr = "";
		const proc = kernel.spawn("tar", args, {
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

	async function createArchive(path = "/project/archive.tar") {
		const result = await runTar(["-cf", path, "-C", "/project", "src"]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
	}

	async function readGuestText(path: string): Promise<string> {
		if (!kernel) throw new Error("kernel not mounted");
		return textDecoder.decode(await kernel.readFile(path));
	}

	it("creates and lists file-backed archives", async () => {
		await mountFixture();
		await createArchive();

		const result = await runTar(["-tf", "/project/archive.tar"]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(lines(result.stdout)).toEqual(
			expect.arrayContaining([
				"src/",
				"src/docs/",
				"src/docs/readme.txt",
				"src/data/",
				"src/data/values.csv",
			]),
		);
	});

	it("extracts archives into target directories", async () => {
		await mountFixture();
		await createArchive();

		const result = await runTar(["-xf", "/project/archive.tar", "-C", "/project/out"]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		await expect(readGuestText("/project/out/src/docs/readme.txt")).resolves.toBe(
			"hello from docs\n",
		);
		await expect(readGuestText("/project/out/src/data/values.csv")).resolves.toBe(
			"name,score\nAda,7\nLinus,3\n",
		);
	});

	it("auto-detects gzip archives by extension", async () => {
		await mountFixture();

		const create = await runTar(["-czf", "/project/archive.tgz", "-C", "/project", "src"]);
		expect(create.exitCode, create.stderr || create.stdout).toBe(0);

		const list = await runTar(["-tf", "/project/archive.tgz"]);
		expect(list.exitCode, list.stderr || list.stdout).toBe(0);
		expect(lines(list.stdout)).toContain("src/docs/readme.txt");

		const extract = await runTar(["-xf", "/project/archive.tgz", "-C", "/project/gzip-out"]);
		expect(extract.exitCode, extract.stderr || extract.stdout).toBe(0);
		await expect(readGuestText("/project/gzip-out/src/docs/readme.txt")).resolves.toBe(
			"hello from docs\n",
		);
	});

	it("strips path components on extraction", async () => {
		await mountFixture();
		await createArchive();

		const result = await runTar([
			"-xf",
			"/project/archive.tar",
			"-C",
			"/project/strip",
			"--strip-components=1",
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		await expect(readGuestText("/project/strip/docs/readme.txt")).resolves.toBe(
			"hello from docs\n",
		);
		await expect(readGuestText("/project/strip/src/docs/readme.txt")).rejects.toThrow();
	});

	it("fails when a create input is missing", async () => {
		await mountFixture();

		const result = await runTar(["-cf", "/project/missing.tar", "-C", "/project", "nope"]);
		expect(result.exitCode).not.toBe(0);
		expect(result.stderr).toContain("nope");
	});
});
