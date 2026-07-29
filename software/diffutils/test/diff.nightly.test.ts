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

const DIFF_COMMAND_DIR = fileURLToPath(new URL("../bin", import.meta.url));
const hasDiffPackageBinary = existsSync(join(DIFF_COMMAND_DIR, "diff"));

let tempRoot: string | undefined;

async function writeFixture(path: string, contents: string): Promise<void> {
	if (!tempRoot) throw new Error("fixture root not initialized");
	const hostPath = join(tempRoot, path.replace(/^\/+/, ""));
	await mkdir(dirname(hostPath), { recursive: true });
	await writeFile(hostPath, contents);
}

async function createTestVFS(): Promise<NodeFileSystem> {
	tempRoot = await mkdtemp(join(tmpdir(), "agentos-diff-"));
	await writeFixture("/project/a.txt", "alpha\nbeta\ngamma\n");
	await writeFixture("/project/b.txt", "alpha\ndelta\ngamma\n");
	await writeFixture("/project/same.txt", "alpha\nbeta\ngamma\n");
	await writeFixture("/project/ws-a.txt", "Alpha\n\nbeta value\n");
	await writeFixture("/project/ws-b.txt", "alpha\n BETA   VALUE\n");
	await writeFixture("/project/left/common.txt", "one\ntwo\nthree\n");
	await writeFixture("/project/right/common.txt", "one\n2\nthree\n");
	await writeFixture("/project/right/extra.txt", "only on right\n");
	return new NodeFileSystem({ root: tempRoot });
}

describeIf(hasDiffPackageBinary, "diff command", { timeout: 10_000 }, () => {
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
		await kernel.mount(createWasmVmRuntime({ commandDirs: [DIFF_COMMAND_DIR] }));
	}

	async function runDiff(args: string[]) {
		if (!kernel) throw new Error("kernel not mounted");
		let stdout = "";
		let stderr = "";
		const proc = kernel.spawn("diff", args, {
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

	it("returns zero for identical files", async () => {
		await mountFixture();

		const result = await runDiff(["/project/a.txt", "/project/same.txt"]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stdout).toBe("");
	});

	it("prints normal diffs for changed files", async () => {
		await mountFixture();

		const result = await runDiff(["/project/a.txt", "/project/b.txt"]);
		expect(result.exitCode).toBe(1);
		expect(result.stdout).toContain("2c2");
		expect(result.stdout).toContain("< beta");
		expect(result.stdout).toContain("> delta");
	});

	it("prints unified diffs", async () => {
		await mountFixture();

		const result = await runDiff(["-u", "/project/a.txt", "/project/b.txt"]);
		expect(result.exitCode).toBe(1);
		expect(result.stdout).toContain("--- /project/a.txt");
		expect(result.stdout).toContain("+++ /project/b.txt");
		expect(result.stdout).toContain("-beta");
		expect(result.stdout).toContain("+delta");
	});

	it("prints brief output with -q", async () => {
		await mountFixture();

		const result = await runDiff(["-q", "/project/a.txt", "/project/b.txt"]);
		expect(result.exitCode).toBe(1);
		expect(result.stdout.trim()).toBe(
			"Files /project/a.txt and /project/b.txt differ",
		);
	});

	it("honors ignore-case, whitespace, and blank-line flags", async () => {
		await mountFixture();

		const result = await runDiff([
			"-i",
			"-w",
			"-B",
			"/project/ws-a.txt",
			"/project/ws-b.txt",
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stdout).toBe("");
	});

	it("compares directories recursively", async () => {
		await mountFixture();

		const result = await runDiff(["-r", "/project/left", "/project/right"]);
		expect(result.exitCode).toBe(1);
		expect(result.stdout).toContain("Only in /project/right: extra.txt");
		expect(result.stdout).toContain("/project/left/common.txt");
		expect(result.stdout).toContain("/project/right/common.txt");
	});

	it("returns an error for missing inputs", async () => {
		await mountFixture();

		const result = await runDiff(["/project/a.txt", "/project/missing.txt"]);
		expect(result.exitCode).toBe(2);
		expect(result.stderr).toContain("/project/missing.txt");
	});
});
