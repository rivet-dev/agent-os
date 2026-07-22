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

const JQ_COMMAND_DIR = fileURLToPath(new URL("../bin", import.meta.url));
const hasJqPackageBinary = existsSync(join(JQ_COMMAND_DIR, "jq"));

let tempRoot: string | undefined;

async function writeFixture(path: string, contents: string): Promise<void> {
	if (!tempRoot) throw new Error("fixture root not initialized");
	const hostPath = join(tempRoot, path.replace(/^\/+/, ""));
	await mkdir(dirname(hostPath), { recursive: true });
	await writeFile(hostPath, contents);
}

async function createTestVFS(): Promise<NodeFileSystem> {
	tempRoot = await mkdtemp(join(tmpdir(), "agentos-jq-"));
	await writeFixture(
		"/project/users.json",
		`${JSON.stringify(
			{
				users: [
					{ name: "Ada", active: true, team: "runtime", score: 7 },
					{ name: "Grace", active: false, team: "docs", score: 5 },
					{ name: "Linus", active: true, team: "runtime", score: 3 },
				],
			},
			null,
			2,
		)}\n`,
	);
	await writeFixture(
		"/project/events.ndjson",
		[
			JSON.stringify({ type: "build", value: 4 }),
			JSON.stringify({ type: "test", value: 6 }),
			JSON.stringify({ type: "deploy", value: 2 }),
		].join("\n") + "\n",
	);
	await writeFixture("/project/broken.json", '{"users": [');
	return new NodeFileSystem({ root: tempRoot });
}

function lines(stdout: string): string[] {
	return stdout.split("\n").filter((line) => line.length > 0);
}

describeIf(hasJqPackageBinary, "jq command", { timeout: wasmBackendTestTimeout(10_000, 30_000) }, () => {
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
		await kernel.mount(createWasmVmRuntime({ commandDirs: [JQ_COMMAND_DIR] }));
	}

	async function runJq(args: string[]) {
		if (!kernel) throw new Error("kernel not mounted");
		let stdout = "";
		let stderr = "";
		const proc = kernel.spawn("jq", args, {
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

	it("reports a jq-compatible version", async () => {
		await mountFixture();

		const result = await runJq(["--version"]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stdout).toMatch(/^jq-/);
	});

	it("filters arrays and emits raw strings", async () => {
		await mountFixture();

		const result = await runJq([
			"-r",
			'.users[] | select(.active) | "\\(.name):\\(.score)"',
			"/project/users.json",
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(lines(result.stdout)).toEqual(["Ada:7", "Linus:3"]);
	});

	it("builds aggregate JSON objects", async () => {
		await mountFixture();

		const result = await runJq([
			"-c",
			'{activeNames: [.users[] | select(.active) | .name], runtimeTotal: ([.users[] | select(.team == "runtime") | .score] | add)}',
			"/project/users.json",
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(JSON.parse(result.stdout.trim())).toEqual({
			activeNames: ["Ada", "Linus"],
			runtimeTotal: 10,
		});
	});

	it("slurps newline-delimited JSON records", async () => {
		await mountFixture();

		const result = await runJq([
			"-s",
			"-c",
			"{count: length, total: (map(.value) | add), types: map(.type)}",
			"/project/events.ndjson",
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(JSON.parse(result.stdout.trim())).toEqual({
			count: 3,
			total: 12,
			types: ["build", "test", "deploy"],
		});
	});

	it("fails with a parse error for invalid JSON", async () => {
		await mountFixture();

		const result = await runJq([".", "/project/broken.json"]);
		expect(result.exitCode).not.toBe(0);
		expect(result.stderr).toContain("parse error");
	});
});
