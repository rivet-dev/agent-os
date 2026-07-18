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

const ACL_COMMAND_DIR = fileURLToPath(new URL("../bin", import.meta.url));
const ACL_COMMANDS = ["chacl", "getfacl", "setfacl"];
const hasAclCommands = ACL_COMMANDS.every((command) =>
	existsSync(join(ACL_COMMAND_DIR, command)),
);

describeIf(hasAclCommands, "ACL commands", { timeout: 30_000 }, () => {
	let filesystem: ReturnType<typeof createInMemoryFileSystem>;
	let kernel: Kernel | undefined;

	beforeEach(async () => {
		filesystem = createInMemoryFileSystem();
		await filesystem.writeFile("/workspace/acl.txt", "acl metadata\n");
		await filesystem.chown("/workspace/acl.txt", 1000, 1000);
		await filesystem.chmod("/workspace/acl.txt", 0o640);
		await filesystem.mkdir("/workspace/defaults", { recursive: true });
		await filesystem.chown("/workspace/defaults", 1000, 1000);
		await filesystem.chmod("/workspace/defaults", 0o750);
		kernel = createKernel({ filesystem });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [ACL_COMMAND_DIR] }));
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

	it("sets an extended access ACL and synchronizes the mode mask", async () => {
		const path = "/workspace/acl.txt";
		const set = await run("setfacl", ["-m", "u:2000:rwx", path]);
		expect(set.exitCode, set.stderr).toBe(0);
		expect(set.stdout).toBe("");
		expect(set.stderr).toBe("");

		const get = await run("getfacl", [
			"-n",
			"--absolute-names",
			path,
		]);
		expect(get.exitCode, get.stderr).toBe(0);
		expect(get.stdout).toContain("# file: /workspace/acl.txt");
		expect(get.stdout).toContain("user::rw-");
		expect(get.stdout).toContain("user:2000:rwx");
		expect(get.stdout).toContain("group::r--");
		expect(get.stdout).toContain("mask::rwx");
		expect(get.stdout).toContain("other::---");
		expect(get.stderr).toBe("");

		const stat = await filesystem.stat(path);
		expect(stat.mode & 0o777).toBe(0o670);
	});

	it("stores a default directory ACL with an automatically calculated mask", async () => {
		const path = "/workspace/defaults";
		const set = await run("setfacl", [
			"-d",
			"-m",
			"u::rwx,u:2000:r--,g::r-x,o::---",
			path,
		]);
		expect(set.exitCode, set.stderr).toBe(0);
		expect(set.stderr).toBe("");

		const get = await run("getfacl", [
			"-n",
			"--absolute-names",
			path,
		]);
		expect(get.exitCode, get.stderr).toBe(0);
		expect(get.stdout).toContain("default:user::rwx");
		expect(get.stdout).toContain("default:user:2000:r--");
		expect(get.stdout).toContain("default:group::r-x");
		expect(get.stdout).toContain("default:mask::r-x");
		expect(get.stdout).toContain("default:other::---");
	});

	it("sets, lists, and removes ACL state through chacl and setfacl", async () => {
		const path = "/workspace/acl.txt";
		const set = await run("chacl", ["u::rw-,g::r--,o::---", path]);
		expect(set.exitCode, set.stderr).toBe(0);
		expect(set.stderr).toBe("");

		const list = await run("chacl", ["-l", path]);
		expect(list.exitCode, list.stderr).toBe(0);
		expect(list.stdout.trim()).toBe(
			"/workspace/acl.txt [u::rw-,g::r--,o::---]",
		);

		const addNamed = await run("setfacl", ["-m", "u:2000:r--", path]);
		expect(addNamed.exitCode, addNamed.stderr).toBe(0);
		const removeAll = await run("setfacl", ["-b", path]);
		expect(removeAll.exitCode, removeAll.stderr).toBe(0);

		const get = await run("getfacl", ["-n", path]);
		expect(get.exitCode, get.stderr).toBe(0);
		expect(get.stdout).not.toContain("user:2000:");
		expect(get.stdout).not.toContain("mask::");
		expect(get.stdout).toContain("user::rw-");
		expect(get.stdout).toContain("group::r--");
		expect(get.stdout).toContain("other::---");
	});
});
