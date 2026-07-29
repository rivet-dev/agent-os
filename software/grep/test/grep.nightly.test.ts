// Nightly: requires a non-core registry command.
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { createWasmVmRuntime } from "@rivet-dev/agentos-test-harness";
import {
	C_BUILD_DIR,
	COMMANDS_DIR,
	NodeFileSystem,
	createKernel,
	describeIf,
	hasCWasmBinaries,
	hasWasmBinaries,
} from "@rivet-dev/agentos-test-harness";
import type { Kernel } from "@rivet-dev/agentos-test-harness";
import { afterEach, describe, expect, it } from "vitest";

let tempRoot: string | undefined;

async function createTestVFS(): Promise<NodeFileSystem> {
	tempRoot = await mkdtemp(join(tmpdir(), "agentos-grep-"));
	await writeFixture(
		"/project/notes.txt",
		["Alpha", "beta", "alphabet", "delta"].join("\n") + "\n",
	);
	await writeFixture(
		"/project/other.txt",
		["gamma", "Beta blocker", "literal a+b"].join("\n") + "\n",
	);
	return new NodeFileSystem({ root: tempRoot });
}

async function writeFixture(path: string, contents: string): Promise<void> {
	if (!tempRoot) throw new Error("fixture root not initialized");
	const hostPath = join(tempRoot, path.replace(/^\/+/, ""));
	await mkdir(dirname(hostPath), { recursive: true });
	await writeFile(hostPath, contents);
}

describeIf(
	hasWasmBinaries && hasCWasmBinaries("grep"),
	"GNU grep command",
	{ timeout: 10_000 },
	() => {
		let kernel: Kernel;

		afterEach(async () => {
			await kernel?.dispose();
			if (tempRoot) {
				await rm(tempRoot, { recursive: true, force: true });
				tempRoot = undefined;
			}
		});

		async function mountFixture(): Promise<NodeFileSystem> {
			const vfs = await createTestVFS();
			kernel = createKernel({ filesystem: vfs });
			await kernel.mount(
				createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
			);
			return vfs;
		}

		it("reports the upstream GNU grep version", async () => {
			await mountFixture();

			const result = await kernel.exec("grep --version", {});
			expect(result.stdout).toContain("GNU grep 3.12");
		});

		it("prints matching lines from a file", async () => {
			await mountFixture();

			const result = await kernel.exec("grep alpha /project/notes.txt", {});
			expect(result.stdout).toBe("alphabet\n");
		});

		it("supports case-insensitive search", async () => {
			await mountFixture();

			const result = await kernel.exec("grep -i beta /project/notes.txt", {});
			expect(result.stdout).toBe("beta\n");
		});

		it("supports inverted matches", async () => {
			await mountFixture();

			const result = await kernel.exec("grep -v Alpha /project/notes.txt", {});
			expect(result.stdout).toBe("beta\nalphabet\ndelta\n");
		});

		it("counts matches", async () => {
			await mountFixture();

			const result = await kernel.exec("grep -c a /project/notes.txt", {});
			expect(result.stdout).toBe("4\n");
		});

		it("prints file names with matches", async () => {
			await mountFixture();

			const result = await kernel.exec(
				"grep -l gamma /project/notes.txt /project/other.txt",
				{},
			);
			expect(result.stdout).toBe("/project/other.txt\n");
		});

		it("supports egrep extended regex alias", async () => {
			await mountFixture();

			const result = await kernel.exec(
				"egrep 'Alpha|delta' /project/notes.txt",
				{},
			);
			expect(result.stdout).toBe("Alpha\ndelta\n");
		});

		it("supports fgrep fixed-string alias", async () => {
			await mountFixture();

			const result = await kernel.exec("fgrep 'a+b' /project/other.txt", {});
			expect(result.stdout).toBe("literal a+b\n");
		});
	},
);
