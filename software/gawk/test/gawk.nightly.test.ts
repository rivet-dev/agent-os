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

const AWK_COMMAND_DIR = fileURLToPath(new URL("../bin", import.meta.url));
const hasAwkPackageBinary = existsSync(join(AWK_COMMAND_DIR, "awk"));

let tempRoot: string | undefined;

async function writeFixture(path: string, contents: string): Promise<void> {
	if (!tempRoot) throw new Error("fixture root not initialized");
	const hostPath = join(tempRoot, path.replace(/^\/+/, ""));
	await mkdir(dirname(hostPath), { recursive: true });
	await writeFile(hostPath, contents);
}

async function createTestVFS(): Promise<NodeFileSystem> {
	tempRoot = await mkdtemp(join(tmpdir(), "agentos-gawk-"));
	await writeFixture(
		"/project/scores.txt",
		[
			"Ada runtime 7",
			"Grace docs 5",
			"Linus runtime 3",
			"Barbara infra 9",
		].join("\n") + "\n",
	);
	await writeFixture(
		"/project/colon.txt",
		["alpha:build:4", "beta:test:6", "gamma:deploy:2"].join("\n") + "\n",
	);
	await writeFixture(
		"/project/runtime.awk",
		'/runtime/ { print $1 ":" $3 }\nEND { print "done" }\n',
	);
	return new NodeFileSystem({ root: tempRoot });
}

function lines(stdout: string): string[] {
	return stdout.split("\n").filter((line) => line.length > 0);
}

describeIf(hasAwkPackageBinary, "awk command", { timeout: 10_000 }, () => {
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
		await kernel.mount(createWasmVmRuntime({ commandDirs: [AWK_COMMAND_DIR] }));
	}

	async function runAwk(args: string[]) {
		if (!kernel) throw new Error("kernel not mounted");
		let stdout = "";
		let stderr = "";
		const proc = kernel.spawn("awk", args, {
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

	it("extracts fields from a file", async () => {
		await mountFixture();

		const result = await runAwk(["{print $1}", "/project/scores.txt"]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(lines(result.stdout)).toEqual(["Ada", "Grace", "Linus", "Barbara"]);
	});

	it("uses explicit field separators", async () => {
		await mountFixture();

		const result = await runAwk(["-F:", "{print $2}", "/project/colon.txt"]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(lines(result.stdout)).toEqual(["build", "test", "deploy"]);
	});

	it("aggregates numeric columns", async () => {
		await mountFixture();

		const result = await runAwk([
			"{sum += $3} END {print sum}",
			"/project/scores.txt",
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stdout.trim()).toBe("24");
	});

	it("runs awk programs from files", async () => {
		await mountFixture();

		const result = await runAwk(["-f", "/project/runtime.awk", "/project/scores.txt"]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(lines(result.stdout)).toEqual(["Ada:7", "Linus:3", "done"]);
	});

	it("fails when an input file is missing", async () => {
		await mountFixture();

		const result = await runAwk(["{print $1}", "/project/missing.txt"]);
		expect(result.exitCode).not.toBe(0);
		expect(result.stderr).toContain("/project/missing.txt");
	});
});
