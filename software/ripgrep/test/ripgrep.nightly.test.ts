// Nightly: requires a non-core registry command.
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { createWasmVmRuntime } from "@rivet-dev/agentos-test-harness";
import {
	COMMANDS_DIR,
	NodeFileSystem,
	createKernel,
	describeIf,
	hasWasmBinaries,
} from "@rivet-dev/agentos-test-harness";
import type { Kernel } from "@rivet-dev/agentos-test-harness";
import { afterEach, describe, expect, it } from "vitest";

let tempRoot: string | undefined;

async function createTestVFS(): Promise<NodeFileSystem> {
	tempRoot = await mkdtemp(join(tmpdir(), "agentos-ripgrep-"));

	await writeFixture(
		"/project/src/main.rs",
		["fn main() {", '    println!("needle");', "}"].join("\n") + "\n",
	);
	await writeFixture(
		"/project/src/lib.rs",
		["pub fn helper() {", "    // Needle in a comment", "}"].join("\n") + "\n",
	);
	await writeFixture("/project/docs/readme.md", "needle in docs\n");
	await writeFixture("/project/vendor/generated.rs", "needle in vendor\n");
	await writeFixture("/project/.hidden.txt", "needle hidden\n");
	await writeFixture("/project/.gitignore", "vendor/\n");
	await writeFixture("/project/.git/HEAD", "ref: refs/heads/main\n");

	return new NodeFileSystem({ root: tempRoot });
}

async function writeFixture(path: string, contents: string): Promise<void> {
	if (!tempRoot) throw new Error("fixture root not initialized");
	const hostPath = join(tempRoot, path.replace(/^\/+/, ""));
	await mkdir(dirname(hostPath), { recursive: true });
	await writeFile(hostPath, contents);
}

function lines(stdout: string): string[] {
	return stdout
		.split("\n")
		.filter((line) => line.length > 0)
		.sort();
}

describeIf(hasWasmBinaries, "ripgrep command", { timeout: 10_000 }, () => {
	let kernel: Kernel;

	afterEach(async () => {
		await kernel?.dispose();
		if (tempRoot) {
			await rm(tempRoot, { recursive: true, force: true });
			tempRoot = undefined;
		}
	});

	async function mountFixture(): Promise<void> {
		const vfs = await createTestVFS();
		kernel = createKernel({ filesystem: vfs });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
	}

	it("reports the upstream ripgrep version", async () => {
		await mountFixture();

		const result = await kernel.exec("rg --version", {});
		expect(result.stdout).toContain("ripgrep 15.1.0");
		expect(result.stdout).not.toContain("agentos");
	});

	it("searches recursively and respects .gitignore by default", async () => {
		await mountFixture();

		const result = await kernel.exec("rg needle /project", {});
		const output = lines(result.stdout);

		expect(output).toContain('/project/src/main.rs:    println!("needle");');
		expect(output).toContain("/project/docs/readme.md:needle in docs");
		expect(output).not.toContain("/project/vendor/generated.rs:needle in vendor");
		expect(output).not.toContain("/project/.hidden.txt:needle hidden");
	});

	it("supports case-insensitive search", async () => {
		await mountFixture();

		const result = await kernel.exec("rg -i needle /project/src", {});
		expect(lines(result.stdout)).toContain("/project/src/lib.rs:    // Needle in a comment");
	});

	it("supports fixed-string search", async () => {
		await mountFixture();

		const result = await kernel.exec("rg -F 'println!(\"needle\")' /project/src", {});
		expect(result.stdout.trim()).toBe('/project/src/main.rs:    println!("needle");');
	});

	it("supports glob filtering", async () => {
		await mountFixture();

		const result = await kernel.exec("rg needle /project -g '*.md'", {});
		expect(result.stdout.trim()).toBe("/project/docs/readme.md:needle in docs");
	});

	it("can include hidden and ignored files when requested", async () => {
		await mountFixture();

		const result = await kernel.exec("rg -uu needle /project", {});
		const output = lines(result.stdout);

		expect(output).toContain("/project/.hidden.txt:needle hidden");
		expect(output).toContain("/project/vendor/generated.rs:needle in vendor");
	});

	it("emits JSON search records", async () => {
		await mountFixture();

		const result = await kernel.exec("rg --json needle /project/docs", {});
		const records = lines(result.stdout).map((line) => JSON.parse(line));

		expect(records.some((record) => record.type === "match")).toBe(true);
		expect(records.some((record) => record.type === "summary")).toBe(true);
	});
});
