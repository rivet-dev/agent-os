import type { BenchmarkOp } from "../lib/layers.js";

function passThroughOp(name: string, sizeBytes: number): BenchmarkOp {
	return {
		family: "pipes",
		name,
		nativeOp: sizeBytes > 16 ? "pipe_throughput" : "pipe_echo",
		nativeArgs: sizeBytes > 16 ? ["--size-bytes", String(sizeBytes)] : undefined,
		wasmUnsupportedReason: "pipe primitives are not supported in the native-baseline wasm lane",
		fileLine: "crates/executor-v8-runtime/src/host_call.rs:276",
		reproducer: `node PassThrough write/read one ${sizeBytes} byte payload inside VM`,
		program: `async () => {
  const { PassThrough } = await import("node:stream");
  const payload = Buffer.alloc(${sizeBytes}, 9);
  await new Promise((resolve, reject) => {
    const stream = new PassThrough();
    const chunks = [];
    stream.on("data", (d) => chunks.push(d));
    stream.on("end", () => {
      const got = Buffer.concat(chunks);
      got.equals(payload) ? resolve() : reject(new Error("bad pipe: " + got.length));
    });
    stream.end(payload);
  });
}`,
	};
}

export const pipesFamily: BenchmarkOp[] = [
	passThroughOp("pass_through_small", 16),
	passThroughOp("pass_through_big", 64 * 1024),
	{
		// Measures backpressure shape and chunk count, not payload-size scaling.
		family: "pipes",
		name: "backpressure_chunks",
		nativeOp: "pipe_backpressure",
		wasmUnsupportedReason: "pipe primitives are not supported in the native-baseline wasm lane",
		fileLine: "crates/executor-v8-runtime/src/host_call.rs:276",
		reproducer: "node PassThrough with a tiny highWaterMark and 64 one-byte writes",
		program: `async () => {
  const { PassThrough } = await import("node:stream");
  await new Promise((resolve, reject) => {
    const stream = new PassThrough({ highWaterMark: 1 });
    let bytes = 0;
    stream.on("data", (d) => bytes += d.length);
    stream.on("end", () => bytes === 64 ? resolve() : reject(new Error(String(bytes))));
    for (let i = 0; i < 64; i++) stream.write(Buffer.from([i & 255]));
    stream.end();
  });
}`,
	},
];
