// Nightly: requires a non-core registry command.
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
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

const VIM_COMMAND_DIR = fileURLToPath(new URL("../bin", import.meta.url));
const VIM_RUNTIME_DIR = fileURLToPath(
	new URL("../dist/package/share/vim/vim92", import.meta.url),
);
const hasVimPackage = existsSync(join(VIM_COMMAND_DIR, "vim")) &&
	existsSync(join(VIM_RUNTIME_DIR, "defaults.vim"));

let tempRoot: string | undefined;

async function writeFixture(path: string, contents: string): Promise<void> {
	if (!tempRoot) throw new Error("fixture root not initialized");
	const hostPath = join(tempRoot, path.replace(/^\/+/, ""));
	await mkdir(dirname(hostPath), { recursive: true });
	await writeFile(hostPath, contents);
}

async function createTestVFS(): Promise<NodeFileSystem> {
	tempRoot = await mkdtemp(join(tmpdir(), "agentos-vim-"));
	await writeFixture("/project/input.txt", "alpha\nbeta\ngamma\n");
	await writeFixture(
		"/project/edit.vim",
		"set nomore\nedit /project/input.txt\n%s/beta/delta/\nwrite\nquitall!\n",
	);
	await cp(VIM_RUNTIME_DIR, join(tempRoot, "usr/local/share/vim/vim92"), {
		recursive: true,
	});
	return new NodeFileSystem({ root: tempRoot });
}

describeIf(hasVimPackage, "vim command", { timeout: 60_000 }, () => {
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
		await kernel.mount(createWasmVmRuntime({ commandDirs: [VIM_COMMAND_DIR] }));
	}

	async function runVim(args: string[]) {
		if (!kernel) throw new Error("kernel not mounted");
		let stdout = "";
		let stderr = "";
		const proc = kernel.spawn("vim", args, {
			streamStdin: true,
			env: {
				TERM: "xterm",
				VIM: "/usr/local/share/vim",
				VIMRUNTIME: "/usr/local/share/vim/vim92",
			},
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});
		proc.closeStdin();
		const exitCode = await proc.wait();
		await new Promise<void>((resolve) => setTimeout(resolve, 0));
		return { stdout, stderr, exitCode };
	}

	it("starts the packaged binary and reports Vim features", async () => {
		await mountFixture();

		const result = await runVim(["--version"]);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stdout).toContain("VIM - Vi IMproved");
		expect(result.stdout).toContain("-libcall");
	});

	it("edits and writes a file in Ex mode", async () => {
		await mountFixture();

		const result = await runVim([
			"-Nu",
			"NONE",
			"-n",
			"-es",
			"-S",
			"/project/edit.vim",
		]);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		if (!kernel) throw new Error("kernel not mounted");
		const edited = Buffer.from(await kernel.readFile("/project/input.txt")).toString(
			"utf8",
		);
		expect(edited).toBe("alpha\ndelta\ngamma\n");
	});
});
