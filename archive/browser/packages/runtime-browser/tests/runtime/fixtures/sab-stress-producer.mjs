// Real-thread producer for the SAB race stress (node:worker_threads). Writes N
// STDOUT frames into an up-ring on a SEPARATE OS thread and bumps the global GEN
// (+ Atomics.notify) after each, exactly as an execution worker would. The format
// mirrors SabRing/sab-reactor exactly (header: head@0,tail@1 Int32; data at byte
// 16; ring frame = [len:u32-le][reactorFrame]; reactorFrame = [kind][payload];
// STDOUT kind = 2). If this diverges from the real SabRing the consumer (which uses
// the real SabRing) fails to read it — so the duplication is self-checking.

import { parentPort, workerData } from "node:worker_threads";

const FRAME_STDOUT = 2;
const HEADER_BYTES = 16;
const { ringSab, genSab, slotCount, slotBytes, count } = workerData;

const ctl = new Int32Array(ringSab, 0, 4); // head, tail, _, _
const bytes = new Uint8Array(ringSab, HEADER_BYTES);
const gen = new Int32Array(genSab, 0, 1);

function tryWrite(frame) {
	const head = Atomics.load(ctl, 0);
	const tail = Atomics.load(ctl, 1);
	if (tail - head >= slotCount) return false; // full
	const slot = (tail % slotCount) * slotBytes;
	const len = frame.byteLength;
	bytes[slot] = len & 0xff;
	bytes[slot + 1] = (len >>> 8) & 0xff;
	bytes[slot + 2] = (len >>> 16) & 0xff;
	bytes[slot + 3] = (len >>> 24) & 0xff;
	bytes.set(frame, slot + 4);
	Atomics.store(ctl, 1, tail + 1); // publish (release)
	return true;
}

function signal() {
	Atomics.add(gen, 0, 1);
	Atomics.notify(gen, 0);
}

// A spin-wait the producer uses when the ring is full (consumer is draining on the
// other thread): bounded so we never hard-spin a core.
const spin = new Int32Array(new SharedArrayBuffer(4));

for (let i = 0; i < count; i++) {
	const frame = new Uint8Array(5);
	frame[0] = FRAME_STDOUT;
	frame[1] = i & 0xff;
	frame[2] = (i >>> 8) & 0xff;
	frame[3] = (i >>> 16) & 0xff;
	frame[4] = (i >>> 24) & 0xff;
	while (!tryWrite(frame)) {
		Atomics.wait(spin, 0, 0, 1); // back off ~1ms, let the consumer drain
	}
	signal();
	// Occasionally pace so the consumer genuinely drains to empty and must block on
	// GEN, exercising the wait/notify wakeup path (the lost-wakeup hazard).
	if (i % 64 === 0) Atomics.wait(spin, 0, 0, 1);
}

parentPort.postMessage({ done: true, count });
