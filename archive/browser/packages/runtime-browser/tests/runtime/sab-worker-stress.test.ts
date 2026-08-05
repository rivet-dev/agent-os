import path from "node:path";
import { fileURLToPath } from "node:url";
import { Worker } from "node:worker_threads";
import { describe, expect, it } from "vitest";
import { sabRingByteLength, type SabRingLayout } from "../../src/sab-ring.js";
import {
	FRAME_STDOUT,
	KernelReactor,
	REACTOR_CONTROL_BYTES,
} from "../../src/sab-reactor.js";

// Real-thread race stress: a producer on a SEPARATE OS thread (node:worker_threads)
// hammers the SAB ring + GEN/notify; the reactor consumes on this thread with REAL
// Atomics.wait. This is the property the spec calls "unverifiable single-threaded":
// if the GEN-bump/notify/wait ordering had a lost-wakeup, the consumer would hang
// (timeout) or drop frames (order/count assert fails).

const here = path.dirname(fileURLToPath(import.meta.url));
const PRODUCER = path.join(here, "fixtures", "sab-stress-producer.mjs");

const LAYOUT: SabRingLayout = { slotCount: 1024, slotBytes: 32 };
const COUNT = 2000;

function seqOf(payload: Uint8Array): number {
	return (
		(payload[0] | (payload[1] << 8) | (payload[2] << 16) | (payload[3] << 24)) >>> 0
	);
}

describe("SAB real-thread race stress (node:worker_threads)", () => {
	it(
		"delivers every frame in order across two real threads (no lost wakeup, no corruption)",
		async () => {
			const ringSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT));
			const genSab = new SharedArrayBuffer(REACTOR_CONTROL_BYTES);
			const downSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT)); // unused for stdout

			const reactor = new KernelReactor({
				controlSab: genSab,
				now: () => Date.now(),
				serviceSyscall: () => new Uint8Array(0),
			});
			reactor.register("e", { upSab: ringSab, downSab, layout: LAYOUT });

			const worker = new Worker(PRODUCER, {
				workerData: {
					ringSab,
					genSab,
					slotCount: LAYOUT.slotCount,
					slotBytes: LAYOUT.slotBytes,
					count: COUNT,
				},
			});
			const workerDone = new Promise<void>((resolve, reject) => {
				worker.on("error", reject);
				worker.on("exit", (code) =>
					code === 0 ? resolve() : reject(new Error(`producer exited ${code}`)),
				);
			});

			const received: number[] = [];
			while (received.length < COUNT) {
				const out = reactor.poll("e", Date.now() + 10_000);
				if (out === null) break; // timeout → lost wakeup / hang
				expect(out.kind).toBe(FRAME_STDOUT);
				received.push(seqOf(out.payload));
			}

			await worker.terminate();
			await workerDone.catch(() => {}); // producer may be terminated mid-flight

			// Every frame arrived, exactly once, in order.
			expect(received.length).toBe(COUNT);
			for (let i = 0; i < COUNT; i++) expect(received[i]).toBe(i);
		},
		30_000,
	);
});
