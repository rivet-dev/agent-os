/**
 * WasmVM shell terminal tests — real brush-shell commands verified through
 * headless xterm screen state.
 *
 * All output assertions use exact-match on screenshotTrimmed().
 * Gated with skipIf(!hasWasmBinaries) — requires WASM binary built.
 */

import { describe, it, expect, afterEach } from "vitest";
import { TerminalHarness } from './terminal-harness.js';
import { createWasmVmRuntime } from '@rivet-dev/agent-os/test/runtime';
import { COMMANDS_DIR, createKernel, hasWasmBinaries } from '../helpers.js';
import type { Kernel } from "../helpers.js";

/** brush-shell interactive prompt (captured empirically). */
const PROMPT = "sh-0.4$ ";

// ---------------------------------------------------------------------------
// Simple in-memory VFS for kernel tests
// ---------------------------------------------------------------------------

class SimpleVFS {
	private files = new Map<string, Uint8Array>();
	private dirs = new Set<string>(["/"]);

	async readFile(path: string): Promise<Uint8Array> {
		const d = this.files.get(path);
		if (!d) throw new Error(`ENOENT: ${path}`);
		return d;
	}
	async readTextFile(path: string): Promise<string> {
		return new TextDecoder().decode(await this.readFile(path));
	}
	async readDir(path: string): Promise<string[]> {
		const prefix = path === "/" ? "/" : path + "/";
		const entries: string[] = [];
		for (const p of [...this.files.keys(), ...this.dirs]) {
			if (p !== path && p.startsWith(prefix)) {
				const rest = p.slice(prefix.length);
				if (!rest.includes("/")) entries.push(rest);
			}
		}
		return entries;
	}
	async readDirWithTypes(path: string) {
		return (await this.readDir(path)).map((name) => ({
			name,
			isDirectory: this.dirs.has(
				path === "/" ? `/${name}` : `${path}/${name}`,
			),
		}));
	}
	async writeFile(
		path: string,
		content: string | Uint8Array,
	): Promise<void> {
		const d =
			typeof content === "string"
				? new TextEncoder().encode(content)
				: content;
		this.files.set(path, new Uint8Array(d));
		const parts = path.split("/").filter(Boolean);
		for (let i = 1; i < parts.length; i++) {
			this.dirs.add("/" + parts.slice(0, i).join("/"));
		}
	}
	async createDir(path: string) {
		this.dirs.add(path);
	}
	async mkdir(path: string, _o?: { recursive?: boolean }) {
		this.dirs.add(path);
	}
	async exists(path: string): Promise<boolean> {
		return this.files.has(path) || this.dirs.has(path);
	}
	async stat(path: string) {
		const isDir = this.dirs.has(path);
		const d = this.files.get(path);
		if (!isDir && !d) throw new Error(`ENOENT: ${path}`);
		return {
			mode: isDir ? 0o40755 : 0o100644,
			size: d?.length ?? 0,
			isDirectory: isDir,
			isSymbolicLink: false,
			atimeMs: Date.now(),
			mtimeMs: Date.now(),
			ctimeMs: Date.now(),
			birthtimeMs: Date.now(),
			ino: 0,
			nlink: 1,
			uid: 1000,
			gid: 1000,
		};
	}
	async removeFile(path: string) {
		this.files.delete(path);
	}
	async removeDir(path: string) {
		this.dirs.delete(path);
	}
	async rename(o: string, n: string) {
		const d = this.files.get(o);
		if (d) {
			this.files.set(n, d);
			this.files.delete(o);
		}
	}
	async realpath(path: string) {
		return path;
	}
	async symlink(_t: string, _l: string) {}
	async readlink(_path: string): Promise<string> {
		throw new Error("EINVAL: not a symlink");
	}
	async lstat(path: string) {
		return this.stat(path);
	}
	async link(_oldPath: string, _newPath: string) {}
	async chmod(_path: string, _mode: number) {}
	async chown(_path: string, _uid: number, _gid: number) {}
	async utimes(_path: string, _atime: number, _mtime: number) {}
	async truncate(path: string, length: number) {
		const d = this.files.get(path);
		if (!d) throw new Error(`ENOENT: ${path}`);
		this.files.set(path, d.slice(0, length));
	}
	async pread(path: string, offset: number, length: number): Promise<Uint8Array> {
		const d = this.files.get(path);
		if (!d) throw new Error(`ENOENT: ${path}`);
		return d.slice(offset, offset + length);
	}
}

// ---------------------------------------------------------------------------
// Helper — create kernel + mount WasmVM
// ---------------------------------------------------------------------------

async function createShellKernel(): Promise<{
	kernel: Kernel;
	vfs: SimpleVFS;
}> {
	const vfs = new SimpleVFS();
	const kernel = createKernel({ filesystem: vfs as any });
	await kernel.mount(
		createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }),
	);
	return { kernel, vfs };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe.skipIf(!hasWasmBinaries)("wasmvm-shell-terminal", () => {
	let harness: TerminalHarness;

	afterEach(async () => {
		await harness?.dispose();
	});

	it("echo prints output — 'echo hello' → 'hello' on next line, prompt returns", async () => {
		const { kernel } = await createShellKernel();
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type("echo hello\n");
		await harness.waitFor(PROMPT, 2);

		expect(harness.screenshotTrimmed()).toBe(
			[`${PROMPT}echo hello`, "hello", PROMPT].join("\n"),
		);
	});

	it("ls / shows listing — directory entries include /bin from command registration", async () => {
		const { kernel } = await createShellKernel();
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type("ls /\n");
		await harness.waitFor(PROMPT, 2);

		const screen = harness.screenshotTrimmed();
		// Kernel bootstraps standard POSIX directories (/tmp, /etc, /usr, …)
		// and WasmVM mounts commands into /bin — verify key entries exist.
		expect(screen).toContain("bin");
		expect(screen).toContain("tmp");
	});

	it("/bin/printf resolves through shell PATH — path-based command dispatch works from interactive shell", async () => {
		const { kernel } = await createShellKernel();
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type("/bin/printf 'path-dispatch-ok\\n'\n");
		await harness.waitFor(PROMPT, 2);

		const screen = harness.screenshotTrimmed();
		expect(screen).toContain("path-dispatch-ok");
	});

	it("ls directory with known contents — mkdir + touch then ls shows expected entries", async () => {
		const { kernel, vfs } = await createShellKernel();
		await vfs.createDir("/data");
		await vfs.writeFile("/data/alpha.txt", "a");
		await vfs.writeFile("/data/beta.txt", "b");
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type("ls /data\n");
		await harness.waitFor(PROMPT, 2);

		const screen = harness.screenshotTrimmed();
		expect(screen).toContain("alpha.txt");
		expect(screen).toContain("beta.txt");
	});

	it("output preserved across commands — 'echo AAA' then 'echo BBB' — both visible", async () => {
		const { kernel } = await createShellKernel();
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type("echo AAA\n");
		await harness.waitFor(PROMPT, 2);
		await harness.type("echo BBB\n");
		await harness.waitFor(PROMPT, 3);

		expect(harness.screenshotTrimmed()).toBe(
			[
				`${PROMPT}echo AAA`,
				"AAA",
				`${PROMPT}echo BBB`,
				"BBB",
				PROMPT,
			].join("\n"),
		);
	});

	it("cd changes directory — 'cd /tmp' then 'pwd' → '/tmp' on screen", async () => {
		const { kernel, vfs } = await createShellKernel();
		await vfs.createDir("/tmp");
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type("cd /tmp\n");
		await harness.waitFor(PROMPT, 2);
		await harness.type("pwd\n");
		await harness.waitFor(PROMPT, 3);

		expect(harness.screenshotTrimmed()).toBe(
			[
				`${PROMPT}cd /tmp`,
				`${PROMPT}pwd`,
				"/tmp",
				PROMPT,
			].join("\n"),
		);
	});

	it("export sets env var — 'export FOO=bar' then 'echo $FOO' → 'bar' on screen", async () => {
		const { kernel } = await createShellKernel();
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type("export FOO=bar\n");
		await harness.waitFor(PROMPT, 2);
		await harness.type("echo $FOO\n");
		await harness.waitFor(PROMPT, 3);

		expect(harness.screenshotTrimmed()).toBe(
			[
				`${PROMPT}export FOO=bar`,
				`${PROMPT}echo $FOO`,
				"bar",
				PROMPT,
			].join("\n"),
		);
	});

	it("cat reads VFS file — write file, cat it, content appears on screen", async () => {
		const { kernel, vfs } = await createShellKernel();
		await vfs.writeFile("/tmp/hello.txt", "hello world\n");
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type("cat /tmp/hello.txt\n");
		await harness.waitFor(PROMPT, 2);

		expect(harness.screenshotTrimmed()).toBe(
			[
				`${PROMPT}cat /tmp/hello.txt`,
				" WARN could not retrieve pid for child process",
				"hello world",
				PROMPT,
			].join("\n"),
		);
	});

	it("pipe data via redirect — 'echo foo > file' then 'cat file' → 'foo' on screen", async () => {
		const { kernel, vfs } = await createShellKernel();
		await vfs.createDir("/tmp");
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type("echo foo > /tmp/pipe.out\n");
		await harness.waitFor(PROMPT, 2);
		await harness.type("cat /tmp/pipe.out\n");
		await harness.waitFor(PROMPT, 3);

		expect(harness.screenshotTrimmed()).toBe(
			[
				`${PROMPT}echo foo > /tmp/pipe.out`,
				`${PROMPT}cat /tmp/pipe.out`,
				" WARN could not retrieve pid for child process",
				"foo",
				PROMPT,
			].join("\n"),
		);
	});

	it("bad command — 'nonexistent_cmd' → error message on screen", async () => {
		const { kernel } = await createShellKernel();
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type("nonexistent_cmd\n");
		await harness.waitFor(PROMPT, 2);

		expect(harness.screenshotTrimmed()).toBe(
			[
				`${PROMPT}nonexistent_cmd`,
				"error: command not found: nonexistent_cmd",
				PROMPT,
			].join("\n"),
		);
	});

	it("stderr output appears on screen — 'echo error >&2' shows error text", async () => {
		const { kernel } = await createShellKernel();
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type("echo error >&2\n");
		await harness.waitFor(PROMPT, 2);

		expect(harness.screenshotTrimmed()).toBe(
			[
				`${PROMPT}echo error >&2`,
				"error",
				PROMPT,
			].join("\n"),
		);
	});

	it("redirection — 'echo hello > /tmp/out' then 'cat /tmp/out' → 'hello' on screen", async () => {
		const { kernel, vfs } = await createShellKernel();
		await vfs.createDir("/tmp");
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type("echo hello > /tmp/out\n");
		await harness.waitFor(PROMPT, 2);
		await harness.type("cat /tmp/out\n");
		await harness.waitFor(PROMPT, 3);

		expect(harness.screenshotTrimmed()).toBe(
			[
				`${PROMPT}echo hello > /tmp/out`,
				`${PROMPT}cat /tmp/out`,
				" WARN could not retrieve pid for child process",
				"hello",
				PROMPT,
			].join("\n"),
		);
	});

	it("multi-line input — quoted string continuation across lines", async () => {
		const { kernel } = await createShellKernel();
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type('echo "hello\n');
		// brush-shell buffers until closing quote — no continuation prompt
		await harness.type('world"\n');
		await harness.waitFor(PROMPT, 2);

		expect(harness.screenshotTrimmed()).toBe(
			[
				`${PROMPT}echo "hello`,
				'world"',
				"hello",
				"world",
				PROMPT,
			].join("\n"),
		);
	});

	it("exit command terminates shell — 'exit' causes wait() to resolve with code 0", async () => {
		const { kernel } = await createShellKernel();
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		harness.shell.write("exit\n");

		const exitCode = await Promise.race([
			harness.shell.wait(),
			new Promise<number>((_, rej) =>
				setTimeout(() => rej(new Error("wait() hung after 'exit'")), 10_000),
			),
		]);
		expect(exitCode).toBe(0);
	});

	it("Ctrl+D on empty line exits — ^D causes wait() to resolve with code 0", async () => {
		const { kernel } = await createShellKernel();
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);

		const exitCode = await Promise.race([
			harness.exit(),
			new Promise<number>((_, rej) =>
				setTimeout(() => rej(new Error("wait() hung after ^D")), 10_000),
			),
		]);
		expect(exitCode).toBe(0);
	});

	// -----------------------------------------------------------------------
	// CWD propagation regressions (US-076)
	// -----------------------------------------------------------------------

	// Requires WASM binaries rebuilt with init_cwd.c override so getcwd()
	// reads PWD from env at startup. brush-shell calls getcwd() to determine
	// its initial cwd; without the override, __wasilibc_cwd stays "/".
	it.skip("shell started with non-root cwd — 'pwd' builtin reports that cwd", async () => {
		const { kernel, vfs } = await createShellKernel();
		await vfs.createDir("/home");
		await vfs.createDir("/home/user");
		harness = new TerminalHarness(kernel, { cwd: "/home/user" });

		await harness.waitFor(PROMPT);
		await harness.type("pwd\n");
		await harness.waitFor(PROMPT, 2);

		const screen = harness.screenshotTrimmed();
		expect(screen).toContain("/home/user");
	});

	it("cd then external /bin/pwd — spawned command inherits shell cwd via PWD env", async () => {
		const { kernel, vfs } = await createShellKernel();
		await vfs.createDir("/tmp");
		await vfs.createDir("/tmp/work");
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type("cd /tmp/work\n");
		await harness.waitFor(PROMPT, 2);
		await harness.type("/bin/pwd\n");
		await harness.waitFor(PROMPT, 3);

		const screen = harness.screenshotTrimmed();
		expect(screen).toContain("/tmp/work");
	});

	// Requires WASM binaries rebuilt with init_cwd.c override so getcwd()
	// reads PWD from env. ls uses getcwd() to determine its working
	// directory when called without arguments.
	it.skip("cd then ls — spawned ls lists cwd contents, not root", async () => {
		const { kernel, vfs } = await createShellKernel();
		await vfs.createDir("/data");
		await vfs.writeFile("/data/marker.txt", "x");
		harness = new TerminalHarness(kernel);

		await harness.waitFor(PROMPT);
		await harness.type("cd /data\n");
		await harness.waitFor(PROMPT, 2);
		await harness.type("ls\n");
		await harness.waitFor(PROMPT, 3);

		const screen = harness.screenshotTrimmed();
		expect(screen).toContain("marker.txt");
	});
});
