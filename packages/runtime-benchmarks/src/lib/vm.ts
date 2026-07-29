import { statSync } from "node:fs";
import {
	NodeRuntime,
	resolveNodeRuntimeSidecarBinary,
	resolveNodeRuntimeCommandsDir,
	SidecarProcess,
	type HostDirectoryMount,
	type NodeRuntimeCreateOptions,
	type NodeRuntimeProcess,
	type NodeRuntimeResourceSnapshot,
	type SidecarSpawnOptions,
	type VirtualDirEntry,
} from "@rivet-dev/agentos-runtime-core";
import { createInMemoryFileSystem } from "@rivet-dev/agentos-runtime-core/test-runtime";
import { hasNativeBaselineWasm, supportsWasmLayer } from "./layers.js";
import type { BenchmarkOp, CommandBenchmarkOp } from "./layers.js";

const NATIVE_BASELINE_WASM_COMMAND = "native-baseline";
const NATIVE_BASELINE_WASM_PREWARM_DIR = "/tmp/native-baseline-wasm";

export interface BenchVmOptions {
	commandsDir?: string;
	loopbackExemptPorts?: number[];
	mounts?: HostDirectoryMount[];
	permissions?: NodeRuntimeCreateOptions["permissions"];
	wasmCommandDirs?: string[];
	sidecar?: SidecarProcess;
}

export interface BenchVmProcess {
	pid: number;
	wait(): Promise<number>;
}

export interface BenchVm {
	writeFile(path: string, content: string | Uint8Array): Promise<void>;
	mkdir(path: string, options?: { recursive?: boolean }): Promise<void>;
	delete(path: string, options?: { recursive?: boolean }): Promise<void>;
	readFile(path: string): Promise<Uint8Array>;
	readDir(path: string): Promise<string[]>;
	readdir(path: string): Promise<string[]>;
	readDirWithTypes(path: string): Promise<VirtualDirEntry[]>;
	exec(
		commandLine: string,
		options?: {
			env?: Record<string, string>;
			cwd?: string;
			stdin?: string | Uint8Array;
			onStdout?: (data: Uint8Array) => void;
			onStderr?: (data: Uint8Array) => void;
		},
	): Promise<{ stdout: string; stderr: string; exitCode: number }>;
	execArgv(
		command: string,
		args: string[],
		options?: {
			env?: Record<string, string>;
			cwd?: string;
			stdin?: string | Uint8Array;
			onStdout?: (data: Uint8Array) => void;
			onStderr?: (data: Uint8Array) => void;
		},
	): Promise<{ stdout: string; stderr: string; exitCode: number }>;
	spawnNodeCapture(
		argsOrProgramPath: string[] | string,
		env?: Record<string, string>,
		options?: {
			onStdout?: (data: Uint8Array) => void;
			onStderr?: (data: Uint8Array) => void;
		},
	): Promise<{ stdout: string; stderr: string; exitCode: number }>;
	spawn(
		command: string,
		args: string[],
		options?: {
			env?: Record<string, string>;
			cwd?: string;
			onStdout?: (data: Uint8Array) => void;
			onStderr?: (data: Uint8Array) => void;
		},
	): BenchVmProcess;
	waitProcess(pid: number): Promise<number>;
	execWasmCommand(
		cmd: string,
		args: string[],
		options?: {
			env?: Record<string, string>;
			cwd?: string;
			stdin?: string | Uint8Array;
			onStdout?: (data: Uint8Array) => void;
			onStderr?: (data: Uint8Array) => void;
		},
	): Promise<{ stdout: string; stderr: string; exitCode: number }>;
	getResourceSnapshot(): Promise<NodeRuntimeResourceSnapshot>;
	dispose(): Promise<void>;
	sidecarPid(): number | null;
}

export interface SidecarBinaryProvenance {
	path: string;
	profile: "debug" | "release" | "unknown";
	mtimeMs: number;
	mtimeIso: string;
	sizeBytes: number;
}

export async function createBenchVm(options: BenchVmOptions = {}): Promise<BenchVm> {
	const runtime = await NodeRuntime.create({
		filesystem: createInMemoryFileSystem(),
		permissions: {
			fs: "allow",
			network: "allow",
			childProcess: "allow",
			process: "allow",
			env: "allow",
			...options.permissions,
		},
		mounts: options.mounts,
		commandsDir: options.commandsDir,
		loopbackExemptPorts: options.loopbackExemptPorts,
		wasmCommandDirs: options.wasmCommandDirs,
		sidecar: options.sidecar,
		// Benchmark VM: opt in to the us-resolution guest clock so sub-ms guest
		// samples are real instead of 1ms-floor artifacts. Never enable this for
		// untrusted workloads (timing side channels); off by default everywhere.
		jsRuntime: { highResolutionTime: true },
	});
	const processes = new Map<number, NodeRuntimeProcess>();

	return {
		writeFile(path, content) {
			return runtime.writeFile(path, content);
		},
		async mkdir(path, options = {}) {
			const args = options.recursive ? ["-p", path] : [path];
			const result = await runtime.execCommand("mkdir", args);
			if (result.exitCode !== 0) {
				throw new Error(`mkdir ${path} exited ${result.exitCode}\n${result.stderr}`);
			}
		},
		async delete(path, options = {}) {
			const args = options.recursive ? ["-rf", path] : [path];
			const result = await runtime.execCommand("rm", args);
			if (result.exitCode !== 0) {
				throw new Error(`rm ${path} exited ${result.exitCode}\n${result.stderr}`);
			}
		},
		readFile(path) {
			return runtime.readFile(path);
		},
		readDir(path) {
			return runtime.readDir(path);
		},
		readdir(path) {
			return runtime.readDir(path);
		},
		readDirWithTypes(path) {
			return runtime.readDirWithTypes(path);
		},
		exec(commandLine, execOptions = {}) {
			return runtime.execCommand("sh", ["-c", commandLine], execOptions);
		},
		execArgv(command, args, execOptions = {}) {
			return runtime.execCommand(command, args, execOptions);
		},
		async spawnNodeCapture(argsOrProgramPath, env, captureOptions = {}) {
			const args =
				typeof argsOrProgramPath === "string"
					? [argsOrProgramPath]
					: argsOrProgramPath;
			return runtime.execCommand("node", args, {
				env,
				onStdout: captureOptions.onStdout,
				onStderr: captureOptions.onStderr,
			});
		},
	spawn(command, args, spawnOptions = {}) {
			const proc = runtime.spawnCommand(command, args, {
				env: spawnOptions.env,
				cwd: spawnOptions.cwd,
				onStdout: spawnOptions.onStdout,
				onStderr: spawnOptions.onStderr,
			});
			processes.set(proc.pid, proc);
			return {
				pid: proc.pid,
				wait: async () => {
					try {
						return await proc.wait();
					} finally {
						processes.delete(proc.pid);
					}
				},
			};
		},
		async waitProcess(pid) {
			const proc = processes.get(pid);
			if (!proc) {
				throw new Error(`unknown benchmark process pid ${pid}`);
			}
			try {
				return await proc.wait();
			} finally {
				processes.delete(pid);
			}
		},
		execWasmCommand(cmd, args, execOptions = {}) {
			return runtime.execCommand(cmd, args, execOptions);
		},
		getResourceSnapshot() {
			return runtime.getResourceSnapshot();
		},
		dispose() {
			return runtime.dispose();
		},
		sidecarPid() {
			return sidecarPidFromRuntime(runtime);
		},
	};
}

/**
 * Prewarms a benchmark VM before timed sampling:
 * 1. run trivial guest Node code to force isolate/bridge/first-exec setup;
 * 2. for native-baseline WASM lanes, run a one-iteration cpu_loop command;
 * 3. for command ops, run one discarded VM-command sample so that command WASM
 *    compilation is outside the measured sample set.
 */
export async function prewarmBenchVm(
	vm: BenchVm,
	op: BenchmarkOp | CommandBenchmarkOp,
): Promise<void> {
	const nodeResult = await vm.spawnNodeCapture(["-e", ""]);
	if (nodeResult.exitCode !== 0) {
		throw new Error(`guest node prewarm exited ${nodeResult.exitCode}\n${nodeResult.stderr}`);
	}

	if (
		!("runHostCmd" in op) &&
		op.nativeOp &&
		!op.wasmUnsupportedReason &&
		supportsWasmLayer(op.nativeOp) &&
		hasNativeBaselineWasm()
	) {
		await vm.mkdir(NATIVE_BASELINE_WASM_PREWARM_DIR, { recursive: true });
		const wasmResult = await vm.execWasmCommand(NATIVE_BASELINE_WASM_COMMAND, [
			"--op",
			"cpu_loop",
			"--iters",
			"1",
			"--warmup",
			"0",
			"--base-dir",
			NATIVE_BASELINE_WASM_PREWARM_DIR,
		]);
		if (wasmResult.exitCode !== 0) {
			throw new Error(
				`native-baseline wasm prewarm exited ${wasmResult.exitCode}\n${wasmResult.stderr}`,
			);
		}
	}

	if ("runHostCmd" in op && !op.skipReason) {
		await op.runVmCmd(vm, 1, 0);
	}
}

export function createBenchSidecar(options: SidecarSpawnOptions = {}): SidecarProcess {
	return SidecarProcess.spawn({
		...options,
		command: options.command ?? resolveNodeRuntimeSidecarBinary(),
	});
}

export function resolveBenchCommandsDir(explicit?: string): string {
	return resolveNodeRuntimeCommandsDir(explicit);
}

export function resolveBenchSidecarProvenance(): SidecarBinaryProvenance {
	const path = resolveNodeRuntimeSidecarBinary();
	const stat = statSync(path);
	return {
		path,
		profile: inferSidecarProfile(path),
		mtimeMs: stat.mtimeMs,
		mtimeIso: formatPacificIso(stat.mtime),
		sizeBytes: stat.size,
	};
}

export function formatSidecarProvenance(
	provenance: SidecarBinaryProvenance,
): string {
	return `Sidecar binary: ${provenance.path} (${provenance.profile}, mtime ${provenance.mtimeIso}, size ${provenance.sizeBytes} bytes)`;
}

function sidecarPidFromRuntime(runtime: NodeRuntime): number | null {
	const kernel = (runtime as unknown as {
		kernel?: {
			client?: {
				child?: { pid?: number };
				protocolClient?: {
					child?: { pid?: number };
					sidecarProcess?: { child?: { pid?: number } };
				};
			};
		};
	}).kernel;
	const pid =
		kernel?.client?.child?.pid ??
		kernel?.client?.protocolClient?.child?.pid ??
		kernel?.client?.protocolClient?.sidecarProcess?.child?.pid;
	return typeof pid === "number" ? pid : null;
}

function inferSidecarProfile(path: string): "debug" | "release" | "unknown" {
	if (path.includes("/release/")) return "release";
	if (path.includes("/debug/")) return "debug";
	return "unknown";
}

export function formatPacificIso(date: Date): string {
	const formatter = new Intl.DateTimeFormat("en-CA", {
		timeZone: "America/Los_Angeles",
		year: "numeric",
		month: "2-digit",
		day: "2-digit",
		hour: "2-digit",
		minute: "2-digit",
		second: "2-digit",
		fractionalSecondDigits: 3,
		hourCycle: "h23",
		timeZoneName: "shortOffset",
	});
	const parts = new Map(
		formatter.formatToParts(date).map((part) => [part.type, part.value]),
	);
	return `${parts.get("year")}-${parts.get("month")}-${parts.get("day")}T${parts.get("hour")}:${parts.get("minute")}:${parts.get("second")}.${parts.get("fractionalSecond")}${isoOffset(parts.get("timeZoneName") ?? "GMT")}`;
}

function isoOffset(shortOffset: string): string {
	if (shortOffset === "GMT" || shortOffset === "UTC") return "Z";
	const match = /^GMT([+-])(\d{1,2})(?::(\d{2}))?$/.exec(shortOffset);
	if (!match) return shortOffset;
	const [, sign, hours, minutes = "00"] = match;
	return `${sign}${hours.padStart(2, "0")}:${minutes}`;
}
