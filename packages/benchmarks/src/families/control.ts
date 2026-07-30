import type { BenchmarkOp } from "../lib/layers.js";

export const controlFamily: BenchmarkOp[] = [
	{
		family: "control",
		name: "cpu_loop",
		nativeOp: "cpu_loop",
		fileLine: "crates/executor-v8-runtime/src/javascript.rs:1741",
		reproducer: "bounded integer loop inside one node process",
		expectedRatio: "control",
		program: `async () => {
  let acc = 0;
  for (let i = 0; i < 2000000; i++) acc = (acc + (i ^ ((acc << 7) >>> 0))) >>> 0;
  if (acc < 0) throw new Error("impossible");
}`,
	},
	{
		family: "control",
		name: "alloc_free",
		nativeOp: "alloc_free",
		fileLine: "crates/executor-v8-runtime/src/javascript.rs:1741",
		reproducer: "allocate and drop one 4MiB Uint8Array inside one node process",
		expectedRatio: "control",
		program: `async () => {
  const data = new Uint8Array(4 * 1024 * 1024);
  for (let i = 0; i < data.length; i++) data[i] = i % 251;
  if (data[10] !== 10) throw new Error("bad alloc");
}`,
	},
];
