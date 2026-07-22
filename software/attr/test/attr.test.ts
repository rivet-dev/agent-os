import { existsSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import {
	createInMemoryFileSystem,
	createKernel,
	createWasmVmRuntime,
	describeIf,
	type Kernel,
} from "@rivet-dev/agentos-test-harness";
import { afterEach, beforeEach, expect, it } from "vitest";

const ATTR_COMMAND_DIR = fileURLToPath(new URL("../bin", import.meta.url));
const ATTR_COMMANDS = ["attr", "getfattr", "setfattr"];
const hasAttrCommands = ATTR_COMMANDS.every((command) =>
	existsSync(join(ATTR_COMMAND_DIR, command)),
);

describeIf(hasAttrCommands, "attr commands", { timeout: 30_000 }, () => {
	let kernel: Kernel | undefined;

	beforeEach(async () => {
		const filesystem = createInMemoryFileSystem();
		await filesystem.writeFile("/workspace/metadata.txt", "metadata\n");
		await filesystem.chown("/workspace/metadata.txt", 1000, 1000);
		kernel = createKernel({ filesystem });
		await kernel.mount(
			createWasmVmRuntime({ commandDirs: [ATTR_COMMAND_DIR] }),
		);
	}, 60_000);

	afterEach(async () => {
		await kernel?.dispose();
		kernel = undefined;
	}, 60_000);

	async function run(command: string, args: string[]) {
		if (!kernel) throw new Error("kernel not mounted");
		let stdout = "";
		let stderr = "";
		const process = kernel.spawn(command, args, {
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});
		const exitCode = await process.wait();
		await new Promise<void>((resolve) => setTimeout(resolve, 0));
		return { exitCode, stdout, stderr };
	}

	it("round-trips text and binary user xattrs through the kernel", async () => {
		const path = "/workspace/metadata.txt";
		const setText = await run("setfattr", [
			"-n",
			"user.agentos",
			"-v",
			"phase-one",
			path,
		]);
		expect(setText.exitCode, setText.stderr).toBe(0);
		expect(setText.stdout).toBe("");
		expect(setText.stderr).toBe("");

		const getText = await run("getfattr", [
			"--only-values",
			"-n",
			"user.agentos",
			path,
		]);
		expect(getText.exitCode, getText.stderr).toBe(0);
		expect(getText.stdout).toBe("phase-one");
		expect(getText.stderr).toBe("");

		const setBinary = await run("setfattr", [
			"-n",
			"user.binary",
			"-v",
			"0x0001ff",
			path,
		]);
		expect(setBinary.exitCode, setBinary.stderr).toBe(0);

		const getBinary = await run("getfattr", [
			"--absolute-names",
			"-n",
			"user.binary",
			"-e",
			"hex",
			path,
		]);
		expect(getBinary.exitCode, getBinary.stderr).toBe(0);
		expect(getBinary.stdout).toContain("# file: /workspace/metadata.txt");
		expect(getBinary.stdout).toContain("user.binary=0x0001ff");
		expect(getBinary.stderr).toBe("");

		const getBinaryBase64 = await run("getfattr", [
			"--absolute-names",
			"-nuser.binary",
			"-ebase64",
			path,
		]);
		expect(getBinaryBase64.exitCode, getBinaryBase64.stderr).toBe(0);
		expect(getBinaryBase64.stdout).toContain("user.binary=0sAAH/");
		expect(getBinaryBase64.stderr).toBe("");
	});

	it("lists and removes xattrs with the legacy attr interface", async () => {
		const path = "/workspace/metadata.txt";
		const set = await run("attr", ["-s", "phase", "-V", "ready", path]);
		expect(set.exitCode, set.stderr).toBe(0);
		expect(set.stdout).toContain('Attribute "phase" set to a 5 byte value');
		expect(set.stdout).toContain("ready");

		const get = await run("attr", ["-g", "phase", path]);
		expect(get.exitCode, get.stderr).toBe(0);
		expect(get.stdout).toContain('Attribute "phase" had a 5 byte value');
		expect(get.stdout).toContain("ready");

		const list = await run("attr", ["-l", path]);
		expect(list.exitCode, list.stderr).toBe(0);
		expect(list.stdout).toContain(
			'Attribute "phase" has a 5 byte value for /workspace/metadata.txt',
		);

		const remove = await run("attr", ["-r", "phase", path]);
		expect(remove.exitCode, remove.stderr).toBe(0);
		expect(remove.stderr).toBe("");

		const missing = await run("attr", ["-g", "phase", path]);
		expect(missing.exitCode).toBe(1);
		expect(missing.stdout).toBe("");
		expect(missing.stderr).toContain(
			'Could not get "phase" for /workspace/metadata.txt',
		);
	});

	it("dumps and restores multiple attributes without losing values", async () => {
		const path = "/workspace/metadata.txt";
		for (const [name, value] of [
			["user.alpha", "first"],
			["user.beta", "0x0002fe"],
		] as const) {
			const result = await run("setfattr", ["-n", name, "-v", value, path]);
			expect(result.exitCode, result.stderr).toBe(0);
		}

		const dump = await run("getfattr", [
			"--absolute-names",
			"-d",
			"-e",
			"hex",
			path,
		]);
		expect(dump.exitCode, dump.stderr).toBe(0);
		expect(dump.stdout).toContain("user.alpha=0x6669727374");
		expect(dump.stdout).toContain("user.beta=0x0002fe");

		for (const name of ["user.alpha", "user.beta"]) {
			const result = await run("setfattr", ["-x", name, path]);
			expect(result.exitCode, result.stderr).toBe(0);
		}

		if (!kernel) throw new Error("kernel not mounted");
		await kernel.writeFile("/workspace/attrs.dump", dump.stdout);
		const restore = await run("setfattr", [
			"--restore",
			"/workspace/attrs.dump",
		]);
		expect(restore.exitCode, restore.stderr).toBe(0);

		const restoredAlpha = await run("getfattr", [
			"--only-values",
			"-n",
			"user.alpha",
			path,
		]);
		expect(restoredAlpha.exitCode, restoredAlpha.stderr).toBe(0);
		expect(restoredAlpha.stdout).toBe("first");

		const restoredBeta = await run("getfattr", [
			"-n",
			"user.beta",
			"-e",
			"hex",
			path,
		]);
		expect(restoredBeta.exitCode, restoredBeta.stderr).toBe(0);
		expect(restoredBeta.stdout).toContain("user.beta=0x0002fe");
	});
});
