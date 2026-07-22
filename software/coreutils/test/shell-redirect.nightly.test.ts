import { describe, it, expect, afterEach } from "vitest";
import { spawnSync } from "node:child_process";
import { chmodSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import {
	createInMemoryFileSystem,
	createKernel,
	createWasmVmRuntime,
	COMMANDS_DIR,
	describeIf,
	hasWasmBinaries,
	type Kernel,
	wasmBackendTestTimeout,
} from '@rivet-dev/agentos-test-harness';

const SHELL_REDIRECT_TEST_TIMEOUT_MS = wasmBackendTestTimeout(15_000, 30_000);

function shellQuote(value: string): string {
	return `'${value.replaceAll("'", `'\\''`)}'`;
}

describeIf(hasWasmBinaries, "wasmvm shell redirects", () => {
	let kernel: Kernel | undefined;

	afterEach(async () => {
		await kernel?.dispose();
		kernel = undefined;
	});

	it("creates a redirected file relative to the changed cwd", async () => {
		const vfs = createInMemoryFileSystem();
		await (vfs as any).chmod("/", 0o1777);
		await vfs.mkdir("/tmp", { recursive: true });
		await (vfs as any).chmod("/tmp", 0o1777);
		kernel = createKernel({
			filesystem: vfs,
			syncFilesystemOnDispose: false,
		});
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

		const result = await kernel.exec(
			'sh -c "mkdir -p /tmp/r && cd /tmp/r && echo hi > a.txt && cat a.txt"',
		);

		expect(result.exitCode, result.stderr).toBe(0);
		expect(result.stdout).toBe("hi\n");
		expect(await vfs.exists("/tmp/r/a.txt")).toBe(true);
	}, SHELL_REDIRECT_TEST_TIMEOUT_MS);

	it("keeps Rust path resolution after guest fd 3 is reused", async () => {
		const vfs = createInMemoryFileSystem();
		await (vfs as any).chmod("/", 0o1777);
		await vfs.mkdir("/tmp", { recursive: true });
		await (vfs as any).chmod("/tmp", 0o1777);
		kernel = createKernel({ filesystem: vfs, syncFilesystemOnDispose: false });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

		const result = await kernel.exec(
			"sh -c 'exec 3>/tmp/fd3-owned; mkdir -p /tmp/rust-path && cd /tmp/rust-path && echo payload > file && cat file; printf descriptor >&3'",
		);
		expect(result.exitCode, result.stderr).toBe(0);
		expect(result.stdout).toBe("payload\n");
		expect(Buffer.from(await kernel.readFile("/tmp/rust-path/file")).toString("utf8")).toBe("payload\n");
		expect(Buffer.from(await kernel.readFile("/tmp/fd3-owned")).toString("utf8")).toBe("descriptor");
	}, SHELL_REDIRECT_TEST_TIMEOUT_MS);

	it("preserves bytes written before stdout is closed", async () => {
		const vfs = createInMemoryFileSystem();
		kernel = createKernel({ filesystem: vfs, syncFilesystemOnDispose: false });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
		const script = "printf 'before-close\\n'; exec 1>&-";

		const native = spawnSync("/bin/sh", ["-c", script], { encoding: "utf8" });
		const wasm = await kernel.exec(`sh -c ${shellQuote(script)}`);

		expect(wasm.exitCode).toBe(native.status);
		expect(wasm.stdout).toBe(native.stdout);
		expect(wasm.stderr).toBe(native.stderr);
	}, SHELL_REDIRECT_TEST_TIMEOUT_MS);

	it("matches native exec PATH lookup, argv, environment, and replacement", async () => {
		const vfs = createInMemoryFileSystem();
		kernel = createKernel({ filesystem: vfs, syncFilesystemOnDispose: false });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
		const script =
			`AGENTOS_EXEC_MARK=from-exec exec sh -c ` +
			`'printf "%s|%s|%s\\n" "$0" "$1" "$AGENTOS_EXEC_MARK"' ` +
			`custom-zero custom-one; printf 'continued\\n'`;

		const native = spawnSync("/bin/sh", ["-c", script], { encoding: "utf8" });
		const wasm = await kernel.exec(`sh -c ${shellQuote(script)}`);

		expect(wasm.exitCode).toBe(native.status);
		expect(wasm.stdout).toBe(native.stdout);
		expect(wasm.stderr).toBe(native.stderr);
		expect(wasm.stdout).toBe("custom-zero|custom-one|from-exec\n");
	}, 30_000);

	it("executes an execute-only WASM image without requiring read permission", async () => {
		const vfs = createInMemoryFileSystem();
		await (vfs as any).chmod("/", 0o1777);
		await vfs.mkdir("/tmp", { recursive: true });
		await (vfs as any).chmod("/tmp", 0o1777);
		kernel = createKernel({ filesystem: vfs, syncFilesystemOnDispose: false });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

		const executeOnlyPath = `/tmp/agentos-exec-only-${process.pid}`;
		await kernel.writeFile(executeOnlyPath, readFileSync(`${COMMANDS_DIR}/sh`));
		await (vfs as any).chmod(executeOnlyPath, 0o111);
		const script =
			`exec ${executeOnlyPath} -c ` +
			`'printf "execute-only\\n"'`;
		const wasm = await kernel.exec(`sh -c ${shellQuote(script)}`);

		expect(wasm.exitCode, wasm.stderr).toBe(0);
		expect(wasm.stdout).toBe("execute-only\n");
		expect(wasm.stderr).toBe("");
	}, 30_000);

	it("matches native exec redirections and inherited descriptors", async () => {
		const vfs = createInMemoryFileSystem();
		await (vfs as any).chmod("/", 0o1777);
		await vfs.mkdir("/tmp", { recursive: true });
		await (vfs as any).chmod("/tmp", 0o1777);
		kernel = createKernel({ filesystem: vfs, syncFilesystemOnDispose: false });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
		const stem = `/tmp/agentos-exec-redir-${process.pid}`;
		const stdoutPath = `${stem}.stdout`;
		const fd3Path = `${stem}.fd3`;
		const fd4Path = `${stem}.fd4`;
		const script =
			`exec sh -c 'printf "stdout\\n"; printf "fd3\\n" >&3; printf "fd4\\n" >&4' ` +
			`> ${stdoutPath} 3> ${fd3Path} 4> ${fd4Path}`;
		rmSync(stdoutPath, { force: true });
		rmSync(fd3Path, { force: true });
		rmSync(fd4Path, { force: true });

		try {
			const native = spawnSync("/bin/sh", ["-c", script], { encoding: "utf8" });
			const nativeStdoutFile = readFileSync(stdoutPath, "utf8");
			const nativeFd3File = readFileSync(fd3Path, "utf8");
			const nativeFd4File = readFileSync(fd4Path, "utf8");
			const wasm = await kernel.exec(`sh -c ${shellQuote(script)}`);

			expect(wasm.exitCode, wasm.stderr).toBe(native.status);
			expect(wasm.stdout).toBe(native.stdout);
			expect(wasm.stderr).toBe(native.stderr);
			expect(Buffer.from(await kernel.readFile(stdoutPath)).toString("utf8")).toBe(nativeStdoutFile);
			expect(Buffer.from(await kernel.readFile(fd3Path)).toString("utf8")).toBe(nativeFd3File);
			expect(Buffer.from(await kernel.readFile(fd4Path)).toString("utf8")).toBe(nativeFd4File);
			expect(nativeStdoutFile).toBe("stdout\n");
			expect(nativeFd3File).toBe("fd3\n");
			expect(nativeFd4File).toBe("fd4\n");
		} finally {
			rmSync(stdoutPath, { force: true });
			rmSync(fd3Path, { force: true });
			rmSync(fd4Path, { force: true });
		}
	}, 30_000);

	it("matches native exec without a command applying redirections", async () => {
		const vfs = createInMemoryFileSystem();
		await (vfs as any).chmod("/", 0o1777);
		await vfs.mkdir("/tmp", { recursive: true });
		await (vfs as any).chmod("/tmp", 0o1777);
		kernel = createKernel({ filesystem: vfs, syncFilesystemOnDispose: false });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
		const outputPath = `/tmp/agentos-exec-no-command-${process.pid}`;
		const script = `exec > ${outputPath}; printf 'after\\n'`;
		rmSync(outputPath, { force: true });

		try {
			const native = spawnSync("/bin/sh", ["-c", script], { encoding: "utf8" });
			const nativeFile = readFileSync(outputPath, "utf8");
			const wasm = await kernel.exec(`sh -c ${shellQuote(script)}`);

			expect(wasm.exitCode).toBe(native.status);
			expect(wasm.stdout).toBe(native.stdout);
			expect(wasm.stderr).toBe(native.stderr);
			expect(Buffer.from(await kernel.readFile(outputPath)).toString("utf8")).toBe(nativeFile);
			expect(nativeFile).toBe("after\n");
		} finally {
			rmSync(outputPath, { force: true });
		}
	}, 30_000);

	it("matches native exec failure statuses", async () => {
		const vfs = createInMemoryFileSystem();
		await (vfs as any).chmod("/", 0o1777);
		await vfs.mkdir("/tmp", { recursive: true });
		await (vfs as any).chmod("/tmp", 0o1777);
		kernel = createKernel({ filesystem: vfs, syncFilesystemOnDispose: false });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

		const missingScript = "exec agentos-command-that-does-not-exist";
		const nativeMissing = spawnSync("/bin/sh", ["-c", missingScript], { encoding: "utf8" });
		const wasmMissing = await kernel.exec(`sh -c ${shellQuote(missingScript)}`);
		expect(wasmMissing.exitCode).toBe(nativeMissing.status);
		expect(wasmMissing.exitCode).toBe(127);

		const noexecPath = `/tmp/agentos-exec-noexec-${process.pid}`;
		const noexecScript = "#!/bin/sh\nprintf 'must-not-run\\n'\n";
		writeFileSync(noexecPath, noexecScript, { mode: 0o644 });
		chmodSync(noexecPath, 0o644);
		await kernel.writeFile(noexecPath, noexecScript);
		await (vfs as any).chmod(noexecPath, 0o644);
		try {
			const command = `exec ${noexecPath}`;
			const nativeNoexec = spawnSync("/bin/sh", ["-c", command], { encoding: "utf8" });
			const wasmNoexec = await kernel.exec(`sh -c ${shellQuote(command)}`);
			expect(wasmNoexec.exitCode).toBe(nativeNoexec.status);
			expect(wasmNoexec.exitCode).toBe(126);
			expect(wasmNoexec.stdout).toBe("");
		} finally {
			rmSync(noexecPath, { force: true });
		}
	}, 30_000);
});
