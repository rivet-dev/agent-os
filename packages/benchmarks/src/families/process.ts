import { spawn, spawnSync } from "node:child_process";
import type { BenchmarkOp } from "../lib/layers.js";
import { runGuestSpawn, runNodeSpawn } from "../lib/layers.js";

const NODE_EXIT_ARGS = ["-e", "process.exit(0)"];
const NODE_CAPTURE_ARGS = ["-e", "process.stdout.write('hi')"];

function runNodeStdoutCapture(iters: number, warmup: number): number[] {
	const samples: number[] = [];
	for (let i = 0; i < warmup + iters; i++) {
		const start = process.hrtime.bigint();
		const result = spawnSync("node", NODE_CAPTURE_ARGS, { encoding: "utf8" });
		const ms = Number(process.hrtime.bigint() - start) / 1e6;
		if (result.status !== 0) {
			throw new Error(`node stdout capture exited ${result.status}`);
		}
		if (result.stdout !== "hi") {
			throw new Error(`node stdout capture mismatch: ${JSON.stringify(result.stdout)}`);
		}
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

async function runGuestStdoutCapture(
	vm: Parameters<NonNullable<BenchmarkOp["runGuest"]>>[0],
	iters: number,
	warmup: number,
): Promise<number[]> {
	const samples: number[] = [];
	for (let i = 0; i < warmup + iters; i++) {
		let stdout = "";
		const start = process.hrtime.bigint();
		const proc = vm.spawn("node", NODE_CAPTURE_ARGS, {
			onStdout: (data) => {
				stdout += Buffer.from(data).toString("utf8");
			},
		});
		const code = await vm.waitProcess(proc.pid);
		const ms = Number(process.hrtime.bigint() - start) / 1e6;
		if (code !== 0) throw new Error(`guest stdout capture exited ${code}`);
		if (stdout !== "hi") {
			throw new Error(`guest stdout capture mismatch: ${JSON.stringify(stdout)}`);
		}
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

async function runNodeStdoutListenerOnly(
	iters: number,
	warmup: number,
): Promise<number[]> {
	const samples: number[] = [];
	for (let i = 0; i < warmup + iters; i++) {
		let bytes = 0;
		const start = process.hrtime.bigint();
		const child = spawn("node", NODE_CAPTURE_ARGS, {
			stdio: ["ignore", "pipe", "ignore"],
		});
		child.stdout.on("data", (chunk: Buffer) => {
			bytes += chunk.length;
		});
		await new Promise<void>((resolve, reject) => {
			child.on("error", reject);
			child.on("exit", (code) =>
				code === 0 ? resolve() : reject(new Error(`exit ${code}`)),
			);
		});
		const ms = Number(process.hrtime.bigint() - start) / 1e6;
		if (bytes !== 2) {
			throw new Error(`node stdout listener byte mismatch: ${bytes}`);
		}
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

async function runGuestStdoutListenerOnly(
	vm: Parameters<NonNullable<BenchmarkOp["runGuest"]>>[0],
	iters: number,
	warmup: number,
): Promise<number[]> {
	const samples: number[] = [];
	for (let i = 0; i < warmup + iters; i++) {
		let bytes = 0;
		const start = process.hrtime.bigint();
		const proc = vm.spawn("node", NODE_CAPTURE_ARGS, {
			onStdout: (data) => {
				bytes += data.byteLength;
			},
		});
		const code = await vm.waitProcess(proc.pid);
		const ms = Number(process.hrtime.bigint() - start) / 1e6;
		if (code !== 0) throw new Error(`guest stdout listener exited ${code}`);
		if (bytes !== 2) {
			throw new Error(`guest stdout listener byte mismatch: ${bytes}`);
		}
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

async function runNodeFanout(iters: number, warmup: number): Promise<number[]> {
	const { spawn } = await import("node:child_process");
	const samples: number[] = [];
	for (let i = 0; i < warmup + iters; i++) {
		const start = process.hrtime.bigint();
		const children = Array.from({ length: 8 }, () =>
			spawn("node", NODE_EXIT_ARGS, { stdio: "ignore" }),
		);
		await Promise.all(
			children.map(
				(child) =>
					new Promise<void>((resolve, reject) => {
						child.on("error", reject);
						child.on("exit", (code) =>
							code === 0 ? resolve() : reject(new Error(`exit ${code}`)),
						);
					}),
			),
		);
		if (i >= warmup) {
			samples.push(Number(process.hrtime.bigint() - start) / 1e6);
		}
	}
	return samples;
}

async function runGuestFanout(vm: Parameters<NonNullable<BenchmarkOp["runGuest"]>>[0], iters: number, warmup: number): Promise<number[]> {
	const samples: number[] = [];
	for (let i = 0; i < warmup + iters; i++) {
		const start = process.hrtime.bigint();
		const children = Array.from({ length: 8 }, () => vm.spawn("node", NODE_EXIT_ARGS));
		await Promise.all(children.map((child) => vm.waitProcess(child.pid)));
		if (i >= warmup) {
			samples.push(Number(process.hrtime.bigint() - start) / 1e6);
		}
	}
	return samples;
}

function spawnStdoutCaptureOp(name: string, sizeBytes: number): BenchmarkOp {
	return {
		family: "process",
		name,
		nativeOp: "node_stdout_capture_2b",
		nativeArgs: ["--size-bytes", String(sizeBytes)],
		wasmUnsupportedReason: "process spawning is not supported in the native-baseline wasm lane",
		fileLine: "crates/executor-v8-runtime/src/adapter_host.rs:296",
		reproducer: `spawn node child writing ${sizeBytes} stdout bytes, capture and verify byte count`,
		program: `async () => {
  const { spawn } = await import("node:child_process");
  const child = spawn("node", ["-e", "process.stdout.write(Buffer.alloc(${sizeBytes}, 55))"], {
    stdio: ["ignore", "pipe", "ignore"],
  });
  const chunks = [];
  child.stdout.on("data", (chunk) => chunks.push(chunk));
  await new Promise((resolve, reject) => {
    child.on("error", reject);
    child.on("close", (code) => code === 0 ? resolve() : reject(new Error("exit " + code)));
  });
  const got = Buffer.concat(chunks);
  if (got.length !== ${sizeBytes}) throw new Error("stdout byte mismatch: " + got.length);
}`,
	};
}

export const processFamily: BenchmarkOp[] = [
	{
		family: "process",
		name: "node_exit",
		nativeOp: "node_exit",
		wasmUnsupportedReason: "process spawning is not supported in the native-baseline wasm lane",
		fileLine: "crates/sidecar/src/execution.rs:5349",
		reproducer: "vm.spawn('node', ['-e', 'process.exit(0)']); waitProcess(pid)",
		runNode: (iters, warmup) => runNodeSpawn(NODE_EXIT_ARGS, iters, warmup),
		runGuest: (vm, iters, warmup) =>
			runGuestSpawn(vm, NODE_EXIT_ARGS, iters, warmup),
	},
	{
		family: "process",
		name: "node_stdout_discard_2b",
		nativeOp: "node_stdout_discard_2b",
		wasmUnsupportedReason: "process spawning is not supported in the native-baseline wasm lane",
		fileLine: "crates/executor-v8-runtime/src/host_call.rs:276",
		reproducer: "spawn child that writes 2 stdout bytes, with stdout ignored",
		runNode: (iters, warmup) => runNodeSpawn(NODE_CAPTURE_ARGS, iters, warmup),
		runGuest: (vm, iters, warmup) =>
			runGuestSpawn(vm, NODE_CAPTURE_ARGS, iters, warmup),
	},
	{
		family: "process",
		name: "exec_capture",
		nativeOp: "node_stdout_capture_2b",
		wasmUnsupportedReason: "process spawning is not supported in the native-baseline wasm lane",
		fileLine: "crates/executor-v8-runtime/src/host_call.rs:276",
		reproducer: "spawn child that writes 2 stdout bytes, capture exact stdout",
		runNode: runNodeStdoutCapture,
		runGuest: runGuestStdoutCapture,
	},
	{
		family: "process",
		name: "node_stdout_listener_only_2b",
		nativeOp: "node_stdout_listener_only_2b",
		wasmUnsupportedReason: "process spawning is not supported in the native-baseline wasm lane",
		fileLine: "crates/executor-v8-runtime/src/host_call.rs:276",
		reproducer: "spawn child that writes 2 stdout bytes, count listener bytes only",
		runNode: runNodeStdoutListenerOnly,
		runGuest: runGuestStdoutListenerOnly,
	},
	{
		family: "process",
		name: "fanout_spawn_8",
		nativeOp: "node_fanout",
		wasmUnsupportedReason: "process spawning is not supported in the native-baseline wasm lane",
		fileLine: "crates/sidecar/src/execution.rs:5349",
		reproducer: "spawn 8 node children concurrently, then wait for all pids",
		runNode: runNodeFanout,
		runGuest: runGuestFanout,
	},
	{
		family: "process",
		name: "wait_reap_storm_8",
		nativeOp: "node_reap_storm",
		wasmUnsupportedReason: "process spawning is not supported in the native-baseline wasm lane",
		fileLine: "crates/vm-kernel/src/process_table.rs:842",
		reproducer: "spawn 8 short-lived node children and reap all exits",
		runNode: runNodeFanout,
		runGuest: runGuestFanout,
	},
	{
		family: "process",
		name: "pipe_chain_3",
		nativeOp: "pipe_chain",
		wasmUnsupportedReason: "shell pipe chains are not supported in the native-baseline wasm lane",
		fileLine: "crates/executor-v8-runtime/src/host_call.rs:276",
		reproducer: "node stream pipeline PassThrough -> PassThrough -> PassThrough",
		program: `async () => {
  const { PassThrough, pipeline } = await import("node:stream");
  const { promisify } = await import("node:util");
  const pipe = promisify(pipeline);
  const a = new PassThrough();
  const b = new PassThrough();
  const c = new PassThrough();
  const chunks = [];
  c.on("data", (chunk) => chunks.push(chunk));
  const done = pipe(a, b, c);
  a.end("hello");
  await done;
  if (Buffer.concat(chunks).toString("utf8") !== "hello") throw new Error("bad pipe chain");
}`,
	},
	spawnStdoutCaptureOp("spawn_stdout_capture_small", 4 * 1024),
	spawnStdoutCaptureOp("spawn_stdout_capture_big", 256 * 1024),
	{
		family: "process",
		name: "spawn_stdin_roundtrip",
		nativeOp: "node_stdin_roundtrip",
		nativeArgs: ["--size-bytes", String(4 * 1024)],
		wasmUnsupportedReason: "process spawning is not supported in the native-baseline wasm lane",
		fileLine: "crates/sidecar/src/filesystem.rs:1284",
		reproducer: "spawn node stdin->stdout pipe, write and capture one 4KiB payload",
		program: `async () => {
  const { spawn } = await import("node:child_process");
  const payload = Buffer.alloc(4096, 9);
  const child = spawn("node", ["-e", "process.stdin.on('data', (chunk) => process.stdout.write(chunk)); process.stdin.on('end', () => process.exit(0));"], {
    stdio: ["pipe", "pipe", "pipe"],
  });
  const chunks = [];
  const errors = [];
  child.stdout.on("data", (chunk) => chunks.push(chunk));
  child.stderr.on("data", (chunk) => errors.push(chunk));
  child.stdin.end(payload);
  await new Promise((resolve, reject) => {
    child.on("error", reject);
    child.on("close", (code) => code === 0 ? resolve() : reject(new Error("exit " + code + ": " + Buffer.concat(errors).toString("utf8"))));
  });
  const got = Buffer.concat(chunks);
  if (!got.equals(payload)) throw new Error("stdin roundtrip mismatch: " + got.length);
}`,
	},
];
