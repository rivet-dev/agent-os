import type { BenchmarkOp } from "../lib/layers.js";

/**
 * Perf-finding regression ops.
 *
 * Each op exercises one hot-path anti-pattern surfaced by the 2026-06-30
 * perf-slop scan (see ~/progress/agentos/2026-06-30-fuzz-perf-optimization-report/
 * perf-slop-scan.md). The `program` runs identically under host Node and inside
 * the guest VM, so `tax.emulation = guest.p50 / node.p50` is the guest overhead
 * the corresponding fix is expected to shrink. Capture these numbers BEFORE the
 * fix lands (baseline) and re-run AFTER (regression guard).
 *
 * Findings already covered by existing benches (no op here):
 *   - host_call.rs:429/435/452/534 (per-syscall lock + String clone) -> sync-bridge-floor
 *   - bridge.rs:1978 (args buffer clone)                             -> sync-bridge-floor --payload-bytes=65536
 *   - execution.rs:897 (loopback peer-pairing 50x10ms retry)         -> net/tcp_connect_close
 *
 * Findings that could NOT be turned into a clean guest-vs-node op (documented so
 * the gap is explicit, not silently dropped):
 *   - v8_host.rs:296 (serialize->deserialize per stream event): stdout/stderr IS
 *     the harness data channel, so flooding the stdio event path overruns the
 *     4096 sidecar event buffer and hangs instead of timing. Needs a
 *     sidecar-internal per-event counter.
 *   - execution.rs:962 (byte-by-byte loopback-TLS read): a guest HTTPS loopback
 *     request currently ERRORS (`ERR_AGENTOS_NODE_SYNC_RPC: loopback TLS transport
 *     read`), so there is no successful transfer to time. The error on that exact
 *     path is itself worth a follow-up.
 *   - execution.rs:19315/19895 (HTTP/2 accept + backpressure polls): node:http2
 *     inside the guest hangs (unsupported), so no request completes to time.
 */

export const perfFindingsFamily: BenchmarkOp[] = [
	{
		// crates/sidecar/src/filesystem.rs:1284 — fs.write stdio handler clones the
		// full write buffer into the Stdout/Stderr event. fs.writeSync(2, ...) forces
		// the write synchronously through the sync bridge so the per-call clone cost
		// is actually on the measured path (async stream .write() buffers and reads
		// as ~0). 8 x 64KiB per iter stays well under the 4096 event-buffer bound.
		family: "perf-finding",
		name: "stdio_writeSync_8x64k",
		nativeOp: "stdio_write_sync",
		nativeArgs: ["--size-bytes", String(64 * 1024), "--chunk-count", "8"],
		wasmUnsupportedReason: "stdio fd writes are not supported in the native-baseline wasm lane",
		fileLine: "crates/sidecar/src/filesystem.rs:1284",
		reproducer: "8 synchronous fs.writeSync(2, 64KiB) inside VM (stdio buffer clone)",
		program: `async () => {
  const fs = await import("node:fs");
  const buf = Buffer.alloc(64 * 1024, 7);
  for (let k = 0; k < 8; k++) fs.writeSync(2, buf);
}`,
	},
	{
		// crates/sidecar/src/execution.rs:2213 — guest Unix-socket accept loop sleeps
		// a fixed 10ms on WouldBlock instead of async readiness. One connect+close
		// pays one accept-latency tick. This is the clearest signal in the set
		// (baseline ~30x guest/node) — the 10ms poll dominates a sub-ms host accept.
		family: "perf-finding",
		name: "unix_accept_latency",
		nativeOp: "unix_connect",
		wasmUnsupportedReason: "Unix-domain sockets are not supported in the native-baseline wasm lane",
		fileLine: "crates/sidecar/src/execution.rs:2213",
		reproducer: "connect+close one Unix-domain socket inside VM (10ms accept poll)",
		program: `async () => {
  const net = await import("node:net");
  const os = await import("node:os");
  const path = await import("node:path");
  const sock = path.join(
    os.tmpdir(),
    "perf-unix-" + process.pid + "-" + Math.random().toString(16).slice(2) + ".sock",
  );
  await new Promise((resolve, reject) => {
    const server = net.createServer((s) => s.end());
    server.on("error", reject);
    server.listen(sock, () => {
      const c = net.connect(sock);
      c.on("error", reject);
      c.on("close", () => server.close(() => resolve()));
      c.end();
    });
  });
}`,
	},
];
