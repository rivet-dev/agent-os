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

const FILE_COMMAND_DIR = fileURLToPath(new URL("../bin", import.meta.url));
const hasFilePackageBinary = existsSync(join(FILE_COMMAND_DIR, "file"));

let tempRoot: string | undefined;

async function writeFixture(path: string, contents: string | Buffer): Promise<void> {
	if (!tempRoot) throw new Error("fixture root not initialized");
	const hostPath = join(tempRoot, path.replace(/^\/+/, ""));
	await mkdir(dirname(hostPath), { recursive: true });
	await writeFile(hostPath, contents);
}

async function createTestVFS(): Promise<NodeFileSystem> {
	tempRoot = await mkdtemp(join(tmpdir(), "agentos-file-"));
	await writeFixture("/project/text.txt", "hello from agentOS\n");
	await writeFixture("/project/data.json", '{ "ok": true }\n');
	await writeFixture("/project/script.sh", "#!/usr/bin/env bash\necho hello\n");
	await writeFixture(
		"/project/image.png",
		Buffer.from([
			0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00,
			0x0d,
		]),
	);
	await writeFixture("/project/empty", "");
	await mkdir(join(tempRoot, "project/dir"), { recursive: true });
	return new NodeFileSystem({ root: tempRoot });
}

describeIf(hasFilePackageBinary, "file command", { timeout: wasmBackendTestTimeout(10_000, 30_000) }, () => {
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
		await kernel.mount(createWasmVmRuntime({ commandDirs: [FILE_COMMAND_DIR] }));
	}

	async function runFile(args: string[], stdin?: string) {
		if (!kernel) throw new Error("kernel not mounted");
		let stdout = "";
		let stderr = "";
		const proc = kernel.spawn("file", args, {
			streamStdin: stdin !== undefined,
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});
		if (stdin !== undefined) {
			proc.writeStdin(stdin);
			proc.closeStdin();
		}
		const exitCode = await proc.wait();
		await new Promise<void>((resolve) => setTimeout(resolve, 0));
		return { stdout, stderr, exitCode };
	}

	it("identifies text, JSON, scripts, images, empty files, and directories", async () => {
		await mountFixture();

		const result = await runFile([
			"/project/text.txt",
			"/project/data.json",
			"/project/script.sh",
			"/project/image.png",
			"/project/empty",
			"/project/dir",
		]);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stdout).toContain("/project/text.txt: ASCII text");
		expect(result.stdout).toContain("/project/data.json: JSON text data");
		expect(result.stdout).toContain(
			"/project/script.sh: bash script, ASCII text executable",
		);
		expect(result.stdout).toContain("/project/image.png: PNG image data");
		expect(result.stdout).toContain("/project/empty: empty");
		expect(result.stdout).toContain("/project/dir: directory");
	});

	it("prints brief descriptions with -b", async () => {
		await mountFixture();

		const result = await runFile(["-b", "/project/data.json"]);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stdout.trim()).toBe("JSON text data");
	});

	it("prints MIME types with -i", async () => {
		await mountFixture();

		const result = await runFile([
			"-i",
			"/project/text.txt",
			"/project/image.png",
			"/project/dir",
		]);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stdout).toContain("/project/text.txt: text/plain");
		expect(result.stdout).toContain("/project/image.png: image/png");
		expect(result.stdout).toContain("/project/dir: inode/directory");
	});

	it("reads stdin when the operand is -", async () => {
		await mountFixture();

		const result = await runFile(
			["-b", "-"],
			"#!/usr/bin/env node\nconsole.log('hello')\n",
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stdout.trim()).toBe("node script, ASCII text executable");
	});

	it("returns an error for missing inputs", async () => {
		await mountFixture();

		const result = await runFile(["/project/missing.txt"]);

		expect(result.exitCode).toBe(1);
		expect(result.stderr).toContain("/project/missing.txt");
	});
});
