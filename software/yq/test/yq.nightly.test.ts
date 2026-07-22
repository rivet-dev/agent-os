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

const YQ_COMMAND_DIR = fileURLToPath(new URL("../bin", import.meta.url));
const hasYqPackageBinary = existsSync(join(YQ_COMMAND_DIR, "yq"));

let tempRoot: string | undefined;

async function writeFixture(path: string, contents: string): Promise<void> {
	if (!tempRoot) throw new Error("fixture root not initialized");
	const hostPath = join(tempRoot, path.replace(/^\/+/, ""));
	await mkdir(dirname(hostPath), { recursive: true });
	await writeFile(hostPath, contents);
}

async function createTestVFS(): Promise<NodeFileSystem> {
	tempRoot = await mkdtemp(join(tmpdir(), "agentos-yq-"));
	await writeFixture(
		"/project/services.yaml",
		[
			"services:",
			"  - name: api",
			"    enabled: true",
			"    port: 8080",
			"  - name: worker",
			"    enabled: false",
			"    port: 9090",
		].join("\n") + "\n",
	);
	await writeFixture(
		"/project/services.json",
		JSON.stringify({ services: [{ name: "api" }, { name: "worker" }] }) + "\n",
	);
	await writeFixture(
		"/project/config.toml",
		["[server]", 'name = "agentos"', "port = 7331", "enabled = true"].join("\n") +
			"\n",
	);
	await writeFixture(
		"/project/inventory.xml",
		'<inventory><item id="a">hammer</item><item id="b">nail</item></inventory>\n',
	);
	await writeFixture("/project/broken.yaml", "services:\n  - name: ok\n    bad");
	return new NodeFileSystem({ root: tempRoot });
}

function lines(stdout: string): string[] {
	return stdout.split("\n").filter((line) => line.length > 0);
}

describeIf(hasYqPackageBinary, "yq command", { timeout: wasmBackendTestTimeout(10_000, 30_000) }, () => {
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
		await kernel.mount(createWasmVmRuntime({ commandDirs: [YQ_COMMAND_DIR] }));
	}

	async function runYq(args: string[]) {
		if (!kernel) throw new Error("kernel not mounted");
		let stdout = "";
		let stderr = "";
		const proc = kernel.spawn("yq", args, {
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

	it("filters YAML files and emits raw strings", async () => {
		await mountFixture();

		const result = await runYq([
			"-r",
			".services[] | select(.enabled) | .name",
			"/project/services.yaml",
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(lines(result.stdout)).toEqual(["api"]);
	});

	it("converts YAML query results to compact JSON", async () => {
		await mountFixture();

		const result = await runYq([
			"-o",
			"json",
			"-c",
			"{names: [.services[].name], ports: [.services[].port]}",
			"/project/services.yaml",
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(JSON.parse(result.stdout.trim())).toEqual({
			names: ["api", "worker"],
			ports: [8080, 9090],
		});
	});

	it("reads JSON files explicitly", async () => {
		await mountFixture();

		const result = await runYq([
			"-p",
			"json",
			"-r",
			".services[].name",
			"/project/services.json",
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(lines(result.stdout)).toEqual(["api", "worker"]);
	});

	it("reads TOML files explicitly", async () => {
		await mountFixture();

		const result = await runYq([
			"-p",
			"toml",
			"-o",
			"json",
			"-c",
			".server",
			"/project/config.toml",
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(JSON.parse(result.stdout.trim())).toEqual({
			name: "agentos",
			port: 7331,
			enabled: true,
		});
	});

	it("reads XML files explicitly", async () => {
		await mountFixture();

		const result = await runYq([
			"-p",
			"xml",
			"-r",
			'.inventory.item[]["#text"]',
			"/project/inventory.xml",
		]);
		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(lines(result.stdout)).toEqual(["hammer", "nail"]);
	});

	it("fails with a parse error for invalid YAML", async () => {
		await mountFixture();

		const result = await runYq([".", "/project/broken.yaml"]);
		expect(result.exitCode).not.toBe(0);
		expect(result.stderr).toContain("invalid YAML");
	});
});
