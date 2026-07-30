import type { BenchmarkOp } from "../lib/layers.js";

/**
 * Timer cadence differential ops.
 *
 * Native timer analogues are intentionally honest rather than exact:
 * - setTimeout rows use std::thread::sleep cadence.
 * - setImmediate uses scheduler yield cadence. That does not model JS task queues,
 *   but it is closer than a CPU loop for measuring turn-taking overhead.
 */

export const timersFamily: BenchmarkOp[] = [
	{
		family: "timers",
		name: "settimeout_zero_x100",
		nativeOp: "sleep_timer",
		nativeArgs: ["--timer-count", "100", "--sleep-ns", "0"],
		fileLine: "crates/v8-runtime/src/asset_cache.rs:4750",
		reproducer: "100 chained setTimeout(0) awaits inside VM",
		program: `async () => {
  for (let k = 0; k < 100; k++) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}`,
	},
	{
		family: "timers",
		name: "settimeout_1ms_x50",
		nativeOp: "sleep_timer",
		nativeArgs: ["--timer-count", "50", "--sleep-ns", "1000000"],
		fileLine: "crates/v8-runtime/src/asset_cache.rs:4750",
		reproducer: "50 chained setTimeout(1) awaits inside VM",
		program: `async () => {
  for (let k = 0; k < 50; k++) {
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
}`,
	},
	{
		family: "timers",
		name: "setimmediate_x1000",
		nativeOp: "yield_loop",
		nativeArgs: ["--timer-count", "1000"],
		fileLine: "crates/v8-runtime/src/asset_cache.rs:4750",
		reproducer: "1000 chained setImmediate awaits inside VM, falling back to setTimeout(0)",
		program: `async () => {
  const schedule = typeof setImmediate === "function"
    ? setImmediate
    : (resolve) => setTimeout(resolve, 0);
  for (let k = 0; k < 1000; k++) {
    await new Promise((resolve) => schedule(resolve));
  }
}`,
	},
];
