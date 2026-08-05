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
	wasmBackendTestTimeout,
} from "@rivet-dev/agentos-test-harness";
import type { Kernel } from "@rivet-dev/agentos-test-harness";
import { afterEach, expect, it } from "vitest";

const SED_COMMAND_DIR = fileURLToPath(new URL("../bin", import.meta.url));
const hasSedPackageBinary = existsSync(join(SED_COMMAND_DIR, "sed"));
const SED_TEST_TIMEOUT_MS = wasmBackendTestTimeout(10_000, 30_000);

let tempRoot: string | undefined;

async function writeFixture(path: string, contents: string): Promise<void> {
	if (!tempRoot) throw new Error("fixture root not initialized");
	const hostPath = join(tempRoot, path.replace(/^\/+/, ""));
	await mkdir(dirname(hostPath), { recursive: true });
	await writeFile(hostPath, contents);
}

async function createTestVFS(): Promise<NodeFileSystem> {
	tempRoot = await mkdtemp(join(tmpdir(), "agentos-sed-"));
	await writeFixture(
		"/project/log.txt",
		[
			"ERROR: disk full",
			"INFO: retrying",
			"ERROR: timeout",
			"DEBUG: ignored",
		].join("\n") + "\n",
	);
	await writeFixture(
		"/project/records.txt",
		["alpha:build:4", "beta:test:6", "gamma:deploy:2"].join("\n") + "\n",
	);
	return new NodeFileSystem({ root: tempRoot });
}

function lines(stdout: string): string[] {
	return stdout.split("\n").filter((line) => line.length > 0);
}

describeIf(
	hasSedPackageBinary,
	"sed command",
	{ timeout: SED_TEST_TIMEOUT_MS },
	() => {
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
		await kernel.mount(createWasmVmRuntime({ commandDirs: [SED_COMMAND_DIR] }));
	}

	async function runSed(args: string[]) {
		if (!kernel) throw new Error("kernel not mounted");
		let stdout = "";
		let stderr = "";
		const proc = kernel.spawn("sed", args, {
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

	it("substitutes text in file operands", async () => {
		await mountFixture();

		const result = await runSed(["s/ERROR/WARN/", "/project/log.txt"]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(lines(result.stdout)).toEqual([
			"WARN: disk full",
			"INFO: retrying",
			"WARN: timeout",
			"DEBUG: ignored",
		]);
	});

	it("prints addressed matches with -n", async () => {
		await mountFixture();

		const result = await runSed(["-n", "/ERROR/p", "/project/log.txt"]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(lines(result.stdout)).toEqual(["ERROR: disk full", "ERROR: timeout"]);
	});

	it("deletes addressed records", async () => {
		await mountFixture();

		const result = await runSed(["/DEBUG/d", "/project/log.txt"]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(lines(result.stdout)).toEqual([
			"ERROR: disk full",
			"INFO: retrying",
			"ERROR: timeout",
		]);
	});

	it("applies multiple expressions in order", async () => {
		await mountFixture();

		const result = await runSed([
			"-e",
			"s/:/ /g",
			"-e",
			"s/^/row /",
			"/project/records.txt",
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(lines(result.stdout)).toEqual([
			"row alpha build 4",
			"row beta test 6",
			"row gamma deploy 2",
		]);
	});

	it("fails when an input file is missing", async () => {
		await mountFixture();

		const result = await runSed(["s/a/b/", "/project/missing.txt"]);
		expect(result.exitCode).not.toBe(0);
		expect(result.stderr).toContain("/project/missing.txt");
	});
	},
);
