import { spawn } from "node:child_process";
import { afterEach, describe, expect, it } from "vitest";
import {
	COMMANDS_DIR,
	createInMemoryFileSystem,
	createKernel,
	createWasmVmRuntime,
	describeIf,
	hasWasmBinaries,
	type Kernel,
} from "@rivet-dev/agentos-test-harness";

const signalExitCode: Record<string, number> = {
	SIGTERM: 143,
};

function runNative(command: string, args: string[] = []): Promise<{
	exitCode: number;
	stdout: string;
	stderr: string;
}> {
	return new Promise((resolve) => {
		const child = spawn(command, args, { stdio: ["ignore", "pipe", "pipe"] });
		let stdout = "";
		let stderr = "";
		child.stdout.on("data", (chunk: Buffer) => { stdout += chunk.toString(); });
		child.stderr.on("data", (chunk: Buffer) => { stderr += chunk.toString(); });
		child.on("close", (code, signal) => resolve({
			exitCode: code ?? (signal ? signalExitCode[signal] ?? 1 : 1),
			stdout,
			stderr,
		}));
	});
}

describeIf(hasWasmBinaries, "upstream kill", () => {
	let kernel: Kernel | undefined;

	afterEach(async () => {
		await kernel?.dispose();
		kernel = undefined;
	});

	async function boot(): Promise<Kernel> {
		const filesystem = createInMemoryFileSystem();
		kernel = createKernel({ filesystem });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
		return kernel;
	}

	it("matches native signal names, numbers, and -- parsing", async () => {
		const vm = await boot();
		for (const args of [["-l", "TERM"], ["-l", "15"]]) {
			const native = await runNative("kill", args);
			const wasm = await vm.exec(`kill ${args.join(" ")}`);
			expect(wasm.exitCode).toBe(native.exitCode);
			expect(wasm.stdout).toBe(native.stdout);
		}

		const nativeProbe = await runNative("sh", ["-c", "env kill -0 -- $$"]);
		const wasmProbe = await vm.exec("sh -c 'env kill -0 -- $$'");
		expect(wasmProbe.exitCode, wasmProbe.stderr).toBe(nativeProbe.exitCode);
		expect(wasmProbe.exitCode).toBe(0);
	}, 20_000);

	it.each(["TERM", "15"])("delivers %s like native", async (signal) => {
		const vm = await boot();
		const script = `env kill -${signal} $$; exit 99`;
		const native = await runNative("sh", ["-c", script]);
		const wasm = await vm.exec(`sh -c '${script}'`);
		expect(wasm.exitCode).toBe(native.exitCode);
		expect(wasm.exitCode).toBe(143);
	}, 20_000);

	it("reports invalid signals and missing processes", async () => {
		const vm = await boot();
		const invalidSignal = await vm.exec("kill -s NOT_A_SIGNAL 2147483647");
		expect(invalidSignal.exitCode).not.toBe(0);
		expect(invalidSignal.stderr).not.toBe("");

		const nativeMissing = await runNative("kill", ["-0", "--", "2147483647"]);
		const wasmMissing = await vm.exec("kill -0 -- 2147483647");
		expect(wasmMissing.exitCode).toBe(nativeMissing.exitCode);
		expect(wasmMissing.exitCode).not.toBe(0);
		expect(wasmMissing.stderr.toLowerCase()).toContain("no such process");
	}, 20_000);
});
